use super::super::Freezer;
use super::*;
use std::fs::{create_dir, create_dir_all, read_link, write};

#[test]
fn copy_ro() {
    let diagnostics_dir = Arc::new(DiagnosticsDir::tempdir().unwrap());
    let mut freezer = Freezer::new(diagnostics_dir.clone());
    // Create a file at relative path a/b/c/d
    let a_b_c = diagnostics_dir.to_absolute_path("a/b/c").unwrap();
    create_dir_all(a_b_c).unwrap();
    let a_b_c_d = diagnostics_dir.to_absolute_path("a/b/c/d").unwrap();
    write(a_b_c_d, "contents\n").unwrap();
    let file = freezer.freeze("a/b/c/d").unwrap().file().unwrap();
    // Copy the file to a/b/z
    let a_b_z = diagnostics_dir.to_absolute_path("a/b/z").unwrap();
    file.copy_ro(&a_b_z, "a/b/z".as_ref()).unwrap();
    assert_eq!(&read_link(a_b_z).unwrap(), "c/d");
    // Copy the file to a/b/y/z
    create_dir(diagnostics_dir.to_absolute_path("a/b/y").unwrap()).unwrap();
    let a_b_y_z = diagnostics_dir.to_absolute_path("a/b/y/z").unwrap();
    file.copy_ro(&a_b_y_z, "a/b/y/z".as_ref()).unwrap();
    assert_eq!(&read_link(a_b_y_z).unwrap(), "../c/d");
    // Copy the file to a/b/c/w/y/z
    create_dir_all(diagnostics_dir.to_absolute_path("a/b/c/w/y").unwrap()).unwrap();
    let a_b_c_w_y_z = diagnostics_dir.to_absolute_path("a/b/c/w/y/z").unwrap();
    file.copy_ro(&a_b_c_w_y_z, "a/b/c/w/y/z".as_ref()).unwrap();
    assert_eq!(&read_link(a_b_c_w_y_z).unwrap(), "../../d");
}

/// Tests functions that retrieve file contents (File::bytes, TextFile::bytes, TextFile::str) under
/// various conditions (uncached, cached but with all Arcs dropped, cached with Arcs still
/// allived).
#[test]
fn content_cache() {
    let diagnostics_dir = Arc::new(DiagnosticsDir::tempdir().unwrap());
    let mut freezer = Freezer::new(diagnostics_dir.clone());
    // Create binary (non-UTF-8) and text (UTF-8) files to test with.
    let binary_path = PathBuf::from_iter([diagnostics_dir.path(), "binary.bin".as_ref()]);
    let text_path = PathBuf::from_iter([diagnostics_dir.path(), "text.txt".as_ref()]);
    write(binary_path, [0xff, 0xfe, 0xfd, 0xfc]).unwrap();
    write(text_path, "contents\n").unwrap();
    let binary_file = freezer.freeze("binary.bin").unwrap().file().unwrap();
    let text_file = freezer.freeze("text.txt").unwrap().file().unwrap();

    // Get the contents of each file. Hang on to the Arcs so we can compare addresses later.
    let binary_arc1 = binary_file.bytes();
    assert_eq!(*binary_arc1, [0xff, 0xfe, 0xfd, 0xfc]);
    let text_arc1 = text_file.bytes();
    assert_eq!(*text_arc1, *b"contents\n");

    // Convert text_file into a TextFile, but keep the original File as well.
    let text_textfile = TextFile::try_from(text_file.clone()).unwrap();

    // Get the contents again (this time with a fresh cache), and verify the addresses match.
    let binary_arc2 = binary_file.bytes();
    assert!(Arc::ptr_eq(&binary_arc2, &binary_arc1));
    let text_arc2 = text_textfile.str().into();
    assert!(Arc::ptr_eq(&text_arc2, &text_arc1));
    let text_arc3 = text_textfile.bytes();
    assert!(Arc::ptr_eq(&text_arc3, &text_arc1));
    let text_arc4 = text_file.bytes();
    assert!(Arc::ptr_eq(&text_arc4, &text_arc1));

    // Drop all the Arcs so the caches no longer hit.
    drop((binary_arc1, binary_arc2));
    drop((text_arc1, text_arc2, text_arc3, text_arc4));

    // Retrieve the contents again and verify they are correct. Also verify the addresses match
    // when queried repeatedly.
    let binary_arc3 = binary_file.bytes();
    assert_eq!(*binary_arc3, [0xff, 0xfe, 0xfd, 0xfc]);
    let binary_arc4 = binary_file.bytes();
    assert!(Arc::ptr_eq(&binary_arc4, &binary_arc3));
    let text_arc5 = text_file.bytes();
    assert_eq!(*text_arc5, *b"contents\n");
    let text_arc6 = text_textfile.bytes();
    assert!(Arc::ptr_eq(&text_arc6, &text_arc5));
    let text_arc7 = text_textfile.str().into();
    assert!(Arc::ptr_eq(&text_arc7, &text_arc5));
    let text_arc8 = text_file.bytes();
    assert!(Arc::ptr_eq(&text_arc8, &text_arc5));
}

/// Tests TryFrom<File> for TextFile.
#[test]
fn textfile_tryfrom_file() {
    let diagnostics_dir = Arc::new(DiagnosticsDir::tempdir().unwrap());
    let mut freezer = Freezer::new(diagnostics_dir.clone());
    // Create binary (non-UTF-8) and text (UTF-8) files to test with.
    let binary_path = PathBuf::from_iter([diagnostics_dir.path(), "binary.bin".as_ref()]);
    let text_path = PathBuf::from_iter([diagnostics_dir.path(), "text.txt".as_ref()]);
    write(binary_path, [0xff, 0xfe, 0xfd, 0xfc]).unwrap();
    write(text_path, "contents\n").unwrap();
    let binary_file = freezer.freeze("binary.bin").unwrap().file().unwrap();
    let text_file = freezer.freeze("text.txt").unwrap().file().unwrap();

    // Try to convert each file into a TextFile. This will cause a cache miss. We save the Arc from
    // text_file so force a cache hit on the next attempt.
    assert!(TextFile::try_from(binary_file.clone()).is_err());
    let arc1 = TextFile::try_from(text_file.clone()).unwrap().str();
    assert_eq!(*arc1, *"contents\n");

    // Try to convert each file into a TextFile again. This should cause a cache hit. We don't have
    // a way to verify that for sure, but we can confirm that the address of the returned str from
    // text_file matches.
    assert!(TextFile::try_from(binary_file).is_err());
    let arc2 = TextFile::try_from(text_file).unwrap().str();
    assert!(Arc::ptr_eq(&arc1, &arc2));
}
