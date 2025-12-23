use super::super::{File, Symlink};
use super::*;
use GetError::{FilesystemLoop, LeavesDir, NotADirectory, NotFound};

/// Utility to make a Dir with the given contents with minimal boilerplate.
fn new_dir<const LEN: usize>(contents: [(&str, DirEntry); LEN]) -> Dir {
    Dir {
        contents: Arc::new(contents.map(|(n, e)| (n.into(), e)).into()),
    }
}

/// Utility to return a new File.
fn new_file() -> File {
    File {
        shared: Arc::new(FileShared {}),
    }
}

/// Utility to return a new Symlink FileEntry.
fn symlink_entry(path: &str) -> DirEntry {
    DirEntry::Symlink(Symlink {
        contents: AsRef::<Path>::as_ref(path).into(),
    })
}

/// Return true if a and b are pointer-equivalent (share the same backing storage).
fn is_same_dir(a: Result<ResolvedEntry, GetError>, b: Dir) -> bool {
    match a {
        Ok(ResolvedEntry::Dir(a)) => Arc::ptr_eq(&a.contents, &b.contents),
        _ => false,
    }
}

/// Return true if a and b are pointer-equivalent (share the same backing storage).
// Note: Ideally we would compare file contents, but these tests were written before File was
// implemented so pointer equality is the best we could do.
fn is_same_file(a: Result<ResolvedEntry, GetError>, b: File) -> bool {
    match a {
        Ok(ResolvedEntry::File(a)) => Arc::ptr_eq(&a.shared, &b.shared),
        _ => false,
    }
}

/// Basic tests for [Dir::get].
#[test]
fn get_basic() {
    let file_a = new_file();
    let subdir1 = new_dir([("a.txt", file_a.clone().into())]);
    let file_b = new_file();
    let dir = new_dir([
        ("subdir1", subdir1.clone().into()),
        ("b.txt", file_b.clone().into()),
        ("symlink", symlink_entry("b.txt")),
        ("absolute_link", symlink_entry("/home/user")),
        (
            "subdir2",
            new_dir([("original_dir", symlink_entry(".."))]).into(),
        ),
        ("trivial_circular", symlink_entry("trivial_circular")),
        (
            "complex_circular",
            symlink_entry("subdir2/original_dir/complex_circular/b.txt"),
        ),
    ]);
    assert!(is_same_dir(dir.get(""), dir.clone()));
    assert!(is_same_dir(dir.get("subdir1"), subdir1.clone()));
    assert!(is_same_file(dir.get("subdir1/a.txt"), file_a.clone()));
    assert!(is_same_dir(dir.get("subdir1/.."), dir.clone()));
    assert!(is_same_dir(dir.get("subdir2/original_dir"), dir.clone()));
    assert!(is_same_dir(
        dir.get("subdir2/original_dir/subdir1"),
        subdir1
    ));
    assert!(is_same_file(dir.get("b.txt"), file_b.clone()));
    assert!(is_same_file(
        dir.get("subdir2/original_dir/subdir1/../b.txt"),
        file_b
    ));
    assert!(is_same_file(dir.get("./subdir1/./a.txt"), file_a));
    assert_eq!(dir.get("nonexistent").err(), Some(NotFound));
    assert_eq!(dir.get("subdir1/../../b.txt").err(), Some(LeavesDir));
    assert_eq!(dir.get("b.txt/subdir1").err(), Some(NotADirectory));
    assert_eq!(dir.get("absolute_link/Documents").err(), Some(LeavesDir));
    assert_eq!(dir.get("trivial_circular").err(), Some(FilesystemLoop));
    assert_eq!(dir.get("complex_circular").err(), Some(FilesystemLoop));
}

/// Test [Dir::get] with a diamond-shaped Dir path (that is, one where the same subdirectory
/// appears under multiple intermediate directories).
#[test]
fn get_diamond() {
    let file_a = new_file();
    let dir_a = new_dir([
        ("file.txt", file_a.clone().into()),
        ("symlink", symlink_entry("../subdir/..")),
    ]);
    let file_b = new_file();
    let dir_b = new_dir([
        ("file.txt", file_b.clone().into()),
        ("subdir", dir_a.clone().into()),
    ]);
    let file_c = new_file();
    let dir_c = new_dir([
        ("file.txt", file_c.clone().into()),
        ("subdir", dir_a.clone().into()),
    ]);
    let file_d = new_file();
    let dir = new_dir([
        ("file.txt", file_d.clone().into()),
        ("dir1", dir_b.clone().into()),
        ("dir2", dir_c.clone().into()),
    ]);
    assert!(is_same_file(
        dir.get("dir1/subdir/symlink/subdir/file.txt"),
        file_a
    ));
    assert!(is_same_file(
        dir.get("dir1/subdir/symlink/file.txt"),
        file_b
    ));
    assert!(is_same_file(
        dir.get("dir2/subdir/symlink/file.txt"),
        file_c.clone()
    ));
    assert!(is_same_file(
        dir.get("dir1/subdir/symlink/../file.txt"),
        file_d
    ));
    // This one puts one path for accessing the symlink into the cache, then accesses it through
    // the other path.
    assert!(is_same_file(
        dir.get("dir1/subdir/symlink/../dir2/subdir/symlink/file.txt"),
        file_c
    ));
}

/// Tests [Dir::get] with a symlink pattern for which the naive lookup algorithm exhibits
/// exponential growth.
#[cfg(not(miri))]
#[test]
fn get_exponential() {
    let file = new_file();
    let dir = new_dir([
        ("file.txt", file.clone().into()),
        ("a", symlink_entry(".")),
        ("b", symlink_entry("a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a")),
        ("c", symlink_entry("b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b")),
        ("d", symlink_entry("c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c")),
        ("e", symlink_entry("d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d")),
        ("f", symlink_entry("e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e")),
        ("g", symlink_entry("f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f")),
        ("h", symlink_entry("g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g")),
        ("i", symlink_entry("h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h")),
        ("j", symlink_entry("i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i")),
        ("k", symlink_entry("j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j")),
    ]);
    assert!(is_same_dir(dir.get("k"), dir.clone()));
    assert!(is_same_file(dir.get("k/file.txt"), file));

    // And a variant that is a loop
    let file = new_file();
    let dir = new_dir([
        ("file.txt", file.clone().into()),
        ("a", symlink_entry(".")),
        ("b", symlink_entry("a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a")),
        ("c", symlink_entry("b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b/b")),
        ("d", symlink_entry("c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c/c")),
        ("e", symlink_entry("d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d/d")),
        ("f", symlink_entry("e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e/e")),
        ("g", symlink_entry("f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f/f")),
        ("h", symlink_entry("g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g/g")),
        ("i", symlink_entry("h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h/h")),
        ("j", symlink_entry("i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i/i")),
        ("k", symlink_entry("j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/j/k")),
    ]);
    assert_eq!(dir.get("k").err(), Some(FilesystemLoop));
}
