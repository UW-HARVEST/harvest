use super::super::{DiagnosticsDir, File, Freezer, Symlink};
use super::*;
use GetError::{FilesystemLoop, LeavesDir, NotADirectory, NotFound};
use std::collections::HashSet;
use std::fs::{create_dir, write};
use std::io::{self, ErrorKind};
use std::os::unix::fs::symlink;
use std::sync::atomic::AtomicBool;
use tempfile::{TempDir, tempdir};

/// Utility to easily build up a Dir with the given contents.
struct DirBuilder {
    tempdir: TempDir,
}

impl DirBuilder {
    pub fn new() -> DirBuilder {
        DirBuilder {
            // TODO: Switch to test_util::tempdir
            tempdir: tempdir().unwrap(),
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
        match Freezer::new(Arc::new(DiagnosticsDir {
            path: self.tempdir.path().canonicalize()?,
            reflink_failed: AtomicBool::new(false),
            tempdir: Some(self.tempdir),
        }))
        .freeze("")?
        {
            DirEntry::Dir(dir) => Ok(dir),
            _ => Err(ErrorKind::NotADirectory.into()),
        }
    }

    fn rel_path<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        PathBuf::from_iter([self.tempdir.path(), path.as_ref()])
    }
}

/// Utility to return the names of the entries of this directory.
fn entry_names(dir: Dir) -> HashSet<OsString> {
    dir.entries().map(|(p, _)| p).collect()
}

/// Panics if `dir` is not a directory with the given entry names.
#[track_caller]
fn assert_dir_contains<const N: usize>(dir: Result<ResolvedEntry, GetError>, contents: [&str; N]) {
    assert_eq!(
        HashSet::<OsString>::from_iter(
            dir.expect("not ok")
                .dir()
                .expect("not a dir")
                .entries()
                .map(|(p, _)| p)
        ),
        HashSet::from_iter(contents.map(From::from)),
    );
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

///// Test [Dir::get] with a diamond-shaped Dir path (that is, one where the same subdirectory
///// appears under multiple intermediate directories).
//#[test]
//fn get_diamond() {
//    let file_a = new_file();
//    let dir_a = new_dir([
//        ("file.txt", file_a.clone().into()),
//        ("symlink", symlink_entry("../subdir/..")),
//    ]);
//    let file_b = new_file();
//    let dir_b = new_dir([
//        ("file.txt", file_b.clone().into()),
//        ("subdir", dir_a.clone().into()),
//    ]);
//    let file_c = new_file();
//    let dir_c = new_dir([
//        ("file.txt", file_c.clone().into()),
//        ("subdir", dir_a.clone().into()),
//    ]);
//    let file_d = new_file();
//    let dir = new_dir([
//        ("file.txt", file_d.clone().into()),
//        ("dir1", dir_b.clone().into()),
//        ("dir2", dir_c.clone().into()),
//    ]);
//    assert!(is_same_file(
//        dir.get("dir1/subdir/symlink/subdir/file.txt"),
//        file_a
//    ));
//    assert!(is_same_file(
//        dir.get("dir1/subdir/symlink/file.txt"),
//        file_b
//    ));
//    assert!(is_same_file(
//        dir.get("dir2/subdir/symlink/file.txt"),
//        file_c.clone()
//    ));
//    assert!(is_same_file(
//        dir.get("dir1/subdir/symlink/../file.txt"),
//        file_d
//    ));
//    // This one puts one path for accessing the symlink into the cache, then accesses it through
//    // the other path.
//    assert!(is_same_file(
//        dir.get("dir1/subdir/symlink/../dir2/subdir/symlink/file.txt"),
//        file_c
//    ));
//}
//
///// Tests [Dir::get] with a symlink pattern for which the naive lookup algorithm exhibits
///// exponential growth.
//#[cfg(not(miri))]
//#[test]
//fn get_exponential() {
//    let file = new_file();
//    let dir = new_dir([
//        ("file.txt", file.clone().into()),
//        ("a", symlink_entry(".")),
//        ("b", symlink_entry("a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a")),
//        ("c", symlink_entry("b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b")),
//        ("d", symlink_entry("c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c")),
//        ("e", symlink_entry("d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d")),
//        ("f", symlink_entry("e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e")),
//        ("g", symlink_entry("f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f")),
//        ("h", symlink_entry("g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g")),
//        ("i", symlink_entry("h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h")),
//        ("j", symlink_entry("i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i")),
//        ("k", symlink_entry("j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j")),
//    ]);
//    assert!(is_same_dir(dir.get("k"), dir.clone()));
//    assert!(is_same_file(dir.get("k/file.txt"), file));
//
//    // And a variant that is a loop
//    let file = new_file();
//    let dir = new_dir([
//        ("file.txt", file.clone().into()),
//        ("a", symlink_entry(".")),
//        ("b", symlink_entry("a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a")),
//        ("c", symlink_entry("b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b")),
//        ("d", symlink_entry("c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c")),
//        ("e", symlink_entry("d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d")),
//        ("f", symlink_entry("e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e")),
//        ("g", symlink_entry("f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f")),
//        ("h", symlink_entry("g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g")),
//        ("i", symlink_entry("h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h")),
//        ("j", symlink_entry("i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i")),
//        ("k", symlink_entry("j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/k")),
//    ]);
//    assert_eq!(dir.get("k").err(), Some(FilesystemLoop));
//}
