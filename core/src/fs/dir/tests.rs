use super::super::{DiagnosticsDir, Freezer, test_util::dir_has_entries};
use super::*;
use GetError::{FilesystemLoop, LeavesDir, NotADirectory, NotFound};
use std::fs::{create_dir, create_dir_all, write};
use std::io;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

/// Utility to easily build up a Dir with the given contents.
struct DirBuilder {
    diagnostics_dir: Arc<DiagnosticsDir>,
}

impl DirBuilder {
    pub fn new() -> DirBuilder {
        DirBuilder {
            diagnostics_dir: Arc::new(DiagnosticsDir::tempdir().unwrap()),
        }
    }

    pub fn add_dir<P: AsRef<Path>>(self, path: P) -> io::Result<DirBuilder> {
        create_dir(self.rel_path(path))?;
        Ok(self)
    }

    pub fn add_file<P: AsRef<Path>>(self, path: P, contents: &str) -> io::Result<DirBuilder> {
        write(self.rel_path(path), contents)?;
        Ok(self)
    }

    pub fn add_symlink<P: AsRef<Path>, T: AsRef<Path>>(
        self,
        path: P,
        target: T,
    ) -> io::Result<DirBuilder> {
        symlink(target, self.rel_path(path))?;
        Ok(self)
    }

    pub fn build(self) -> io::Result<Dir> {
        Ok(Freezer::new(self.diagnostics_dir)
            .freeze("")?
            .dir()
            .unwrap())
    }

    /// Appends `path` to diagnostics_dir's path.
    fn rel_path<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        PathBuf::from_iter([self.diagnostics_dir.path(), path.as_ref()])
    }
}

/// Panics if `dir` is not a directory with the given entry names (used for `Dir::get` tests).
#[track_caller]
fn assert_dir_contains<const N: usize>(dir: Result<ResolvedEntry, GetError>, entries: [&str; N]) {
    assert!(dir_has_entries(
        &dir.expect("not ok").dir().expect("not a dir"),
        &entries
    ))
}

/// Panics if `file` is not a file with the given contents.
#[track_caller]
fn assert_file_contains(file: Result<ResolvedEntry, GetError>, contents: &str) {
    assert_eq!(
        &*file.expect("not ok").file().expect("not a file").bytes(),
        contents.as_bytes()
    );
}

/// Basic tests for [Dir::get].
#[test]
fn get_basic() -> io::Result<()> {
    let dir = DirBuilder::new()
        .add_dir("subdir1")?
        .add_file("subdir1/a.txt", "a")?
        .add_file("b.txt", "b")?
        .add_symlink("symlink", "b.txt")?
        .add_symlink("absolute_link", "/home/user")?
        .add_dir("subdir2")?
        .add_symlink("subdir2/original_dir", "..")?
        .add_symlink("trivial_circular", "trivial_circular")?
        .add_symlink(
            "complex_circular",
            "subdir2/original_dir/complex_circular/b.txt",
        )?
        .build()?;
    let root_names = [
        "subdir1",
        "b.txt",
        "symlink",
        "absolute_link",
        "subdir2",
        "trivial_circular",
        "complex_circular",
    ];
    assert_dir_contains(dir.get(""), root_names);
    assert_dir_contains(dir.get("subdir1"), ["a.txt"]);
    assert_file_contains(dir.get("subdir1/a.txt"), "a");
    assert_dir_contains(dir.get("subdir1/.."), root_names);
    assert_file_contains(dir.get("symlink"), "b");
    assert_dir_contains(dir.get("subdir2/original_dir"), root_names);
    assert_dir_contains(dir.get("subdir2/original_dir/subdir1"), ["a.txt"]);
    assert_file_contains(dir.get("b.txt"), "b");
    assert_file_contains(dir.get("subdir2/original_dir/subdir1/../b.txt"), "b");
    assert_file_contains(dir.get("./subdir1/./a.txt"), "a");
    assert_eq!(dir.get("nonexistent").err(), Some(NotFound));
    assert_eq!(dir.get("subdir1/../../b.txt").err(), Some(LeavesDir));
    assert_eq!(dir.get("b.txt/subdir1").err(), Some(NotADirectory));
    assert_eq!(dir.get("absolute_link/Documents").err(), Some(LeavesDir));
    assert_eq!(dir.get("trivial_circular").err(), Some(FilesystemLoop));
    assert_eq!(dir.get("complex_circular").err(), Some(FilesystemLoop));
    Ok(())
}

/// Test [Dir::get] with a diamond-shaped Dir path (that is, one where the same subdirectory
/// appears under multiple intermediate directories).
#[test]
fn get_diamond() {
    // Create the following directory structure:
    //
    // dir1/subdir/file.txt  File with contents "A\n"
    // dir1/subdir/symlink   Symlink with target "../subdir/.."
    // dir1/file.txt         File with contents "B\n"
    // dir2/subdir/          Copy of dir1/subdir
    // dir2/file.txt         File with contents "C\n"
    // file.txt              File with contents "D\n"
    //
    // On the filesystem, this makes dir1/subdir and dir2/subdir distinct subdirectories with
    // identical contents. However, in memory, this makes them two Arc<> references to the same
    // Dir.
    let diagnostics_dir = Arc::new(DiagnosticsDir::tempdir().unwrap());
    let mut freezer = Freezer::new(diagnostics_dir.clone());
    create_dir_all(diagnostics_dir.to_absolute_path("dir1/subdir").unwrap()).unwrap();
    let dir1_subdir_file = diagnostics_dir
        .to_absolute_path("dir1/subdir/file.txt")
        .unwrap();
    let dir1_subdir_symlink = diagnostics_dir
        .to_absolute_path("dir1/subdir/symlink")
        .unwrap();
    let dir1_file = diagnostics_dir.to_absolute_path("dir1/file.txt").unwrap();
    write(dir1_subdir_file, "A\n").unwrap();
    symlink("../subdir/..", dir1_subdir_symlink).unwrap();
    write(dir1_file, "B\n").unwrap();
    create_dir(diagnostics_dir.to_absolute_path("dir2").unwrap()).unwrap();
    let subdir = freezer.freeze("dir1/subdir").unwrap();
    freezer.copy_ro("dir2/subdir", subdir).unwrap();
    let dir2_file = diagnostics_dir.to_absolute_path("dir2/file.txt").unwrap();
    write(dir2_file, "C\n").unwrap();
    write(diagnostics_dir.to_absolute_path("file.txt").unwrap(), "D\n").unwrap();
    let dir = freezer.freeze("").unwrap().dir().unwrap();
    // Verify that dir1/subdir and dir2/subdir are the same pointer.
    let dir1 = dir.get_entry("dir1").unwrap().dir().unwrap();
    let dir2 = dir.get_entry("dir2").unwrap().dir().unwrap();
    assert!(Arc::ptr_eq(
        &dir1.get_entry("subdir").unwrap().dir().unwrap().contents,
        &dir2.get_entry("subdir").unwrap().dir().unwrap().contents,
    ));
    // Helper to return the file contents at the given path.
    let contents = |path| dir.get(path).unwrap().file().unwrap().bytes();
    // Basic test cases
    assert_eq!(*contents("dir1/subdir/symlink/subdir/file.txt"), *b"A\n");
    assert_eq!(*contents("dir1/subdir/symlink/file.txt"), *b"B\n");
    assert_eq!(*contents("dir2/subdir/symlink/file.txt"), *b"C\n");
    assert_eq!(*contents("dir1/subdir/symlink/../file.txt"), *b"D\n");
    // This one puts one path for accessing the symlink into the cache, then accesses it through
    // the other path.
    assert_eq!(
        *contents("dir1/subdir/symlink/../dir2/subdir/symlink/file.txt"),
        *b"C\n"
    );
}

/// Tests [Dir::get] with a symlink pattern for which the naive lookup algorithm exhibits
/// exponential growth.
#[cfg(not(miri))]
#[test]
fn get_exponential() -> io::Result<()> {
    let dir = DirBuilder::new()
        .add_file("file", "contents")?
        .add_symlink("a", ".")?
        .add_symlink("b", "a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a")?
        .add_symlink("c", "b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b")?
        .add_symlink("d", "c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c")?
        .add_symlink("e", "d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d")?
        .add_symlink("f", "e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e")?
        .add_symlink("g", "f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f")?
        .add_symlink("h", "g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g")?
        .add_symlink("i", "h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h")?
        .add_symlink("j", "i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i")?
        .add_symlink("k", "j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j")?
        .build()?;
    assert_dir_contains(
        dir.get("k"),
        [
            "file", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k",
        ],
    );
    assert_file_contains(dir.get("k/file"), "contents");

    // And a variant that is a loop
    let dir = DirBuilder::new()
        .add_file("file", "contents")?
        .add_symlink("a", ".")?
        .add_symlink("b", "a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a")?
        .add_symlink("c", "b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b")?
        .add_symlink("d", "c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c")?
        .add_symlink("e", "d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d")?
        .add_symlink("f", "e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e")?
        .add_symlink("g", "f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f")?
        .add_symlink("h", "g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g")?
        .add_symlink("i", "h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h")?
        .add_symlink("j", "i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i")?
        .add_symlink("k", "j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/k")?
        .build()?;
    assert_eq!(dir.get("k").err(), Some(FilesystemLoop));
    Ok(())
}

#[test]
fn get_nofollow() -> io::Result<()> {
    use GetNofollowError::*;
    let dir = DirBuilder::new()
        .add_dir("subdir")?
        .add_file("file", "")?
        .add_symlink("subdir/symlink", "..")?
        .build()?;
    assert_eq!(dir.get_nofollow("/absolute").err(), Some(LeavesDir));
    assert_eq!(dir.get_nofollow(".").err(), Some(NotADirectory));
    assert_eq!(dir.get_nofollow("subdir/..").err(), Some(NotADirectory));
    assert_eq!(dir.get_nofollow("file/c").err(), Some(NotADirectory));
    assert_eq!(
        dir.get_nofollow("subdir/symlink/file").err(),
        Some(NotADirectory)
    );
    assert_eq!(dir.get_nofollow("nonexistent").err(), Some(NotFound));
    assert_eq!(dir.get_nofollow("subdir/nonexistent").err(), Some(NotFound));
    assert!(dir_has_entries(&dir, &["file", "subdir"]));
    assert!(dir.get_nofollow("file").unwrap().file().is_some());
    assert!(dir_has_entries(
        &dir.get_nofollow("subdir").unwrap().dir().unwrap(),
        &["symlink"]
    ));
    let entry = dir.get_nofollow("subdir/symlink").unwrap();
    assert_eq!(entry.symlink().unwrap().contents(), "..");
    Ok(())
}
