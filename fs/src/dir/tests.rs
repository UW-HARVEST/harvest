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
    assert_eq!(
        entry_names(dir.get("").unwrap().dir().unwrap()),
        HashSet::from(
            [
                "subdir1",
                "b.txt",
                "symlink",
                "absolute_link",
                "subdir2",
                "trivial_circular",
                "complex_circular"
            ]
            .map(From::from)
        )
    );
    assert_eq!(
        entry_names(dir.get("subdir1").unwrap().dir().unwrap()),
        HashSet::from(["a.txt".into()])
    );
    assert_eq!(
        *dir.get("subdir1/a.txt").unwrap().file().unwrap().bytes(),
        *b"a"
    );
    //assert!(is_same_dir(dir.get("subdir1/.."), dir.clone()));
    //assert!(is_same_dir(dir.get("subdir2/original_dir"), dir.clone()));
    //assert!(is_same_dir(
    //    dir.get("subdir2/original_dir/subdir1"),
    //    subdir1
    //));
    //assert!(is_same_file(dir.get("b.txt"), file_b.clone()));
    //assert!(is_same_file(
    //    dir.get("subdir2/original_dir/subdir1/../b.txt"),
    //    file_b
    //));
    //assert!(is_same_file(dir.get("./subdir1/./a.txt"), file_a));
    //assert_eq!(dir.get("nonexistent").err(), Some(NotFound));
    //assert_eq!(dir.get("subdir1/../../b.txt").err(), Some(LeavesDir));
    //assert_eq!(dir.get("b.txt/subdir1").err(), Some(NotADirectory));
    //assert_eq!(dir.get("absolute_link/Documents").err(), Some(LeavesDir));
    //assert_eq!(dir.get("trivial_circular").err(), Some(FilesystemLoop));
    //assert_eq!(dir.get("complex_circular").err(), Some(FilesystemLoop));
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
