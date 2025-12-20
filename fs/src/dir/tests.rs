use super::super::{File, FileShared, Symlink};
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
    assert!(is_same_file(dir.get("subdir1/a.txt"), file_a));
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
fn get_diamond() {}

// TODO: Test cases:
// 2. Exponential symlinks, i.e.:
//    a
//    b -> .
//    c -> b/b/b/b/b/b/b/b/b/b
//    d -> c/c/c/c/c/c/c/c/c/c
//    ...
// 3. Circular symlinks, including circular variants of the previous one.
// 6. Embed one Dir into two different Dirs, evaluate a symlink that uses .. to point to
//    different locations in each parent dir.
