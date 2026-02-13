use super::{DiagnosticsDir, Dir, DirEntry, File, Symlink, dir::GetNofollowError};
use std::collections::HashMap;
use std::fs::{read_dir, symlink_metadata};
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// `Freezer` implements freezing filesystem objects (making them readonly and constructing a
/// `DirEntry` pointing to them -- see the `fs` module documentation for more information). To
/// avoid repeatedly freezing the same directories, it keeps track of which filesystem objects have
/// already been frozen.
pub(crate) struct Freezer {
    diagnostics_dir: Arc<DiagnosticsDir>,
    // Paths are relative to the diagnostic directory, and do not contain symlinks, `.`, or `..`.
    // Nested frozen paths are removed.
    frozen: HashMap<PathBuf, DirEntry>,
}

impl Freezer {
    pub fn new(diagnostics_dir: Arc<DiagnosticsDir>) -> Freezer {
        Freezer {
            diagnostics_dir,
            frozen: HashMap::new(),
        }
    }

    /// Makes a read-only copy of a filesystem object in the diagnostic directory. `path` must be
    /// relative to the diagnostics directory, and cannot contain `.`, `..`, or symlinks.
    pub fn copy_ro<P: AsRef<Path>>(&mut self, path: P, entry: DirEntry) -> io::Result<()> {
        let _ = (path, entry);
        todo!()
    }

    /// Makes a read-write copy of a filesystem object in the diagnostic directory. `path` must be
    /// relative to the diagnostics directory, and cannot contain `.`, `..`, or symlinks.
    pub fn copy_rw<P: AsRef<Path>>(&mut self, path: P, entry: DirEntry) -> io::Result<()> {
        let _ = (path, entry);
        todo!()
    }

    /// Freezes the given path, returning an object referencing it. `path` must be relative to the
    /// diagnostics directory, and cannot contain `.` or `..`. This will not follow symlinks (i.e.
    /// `path` cannot have symlinks in its directory path, and if `path` points to a symlink then a
    /// Symlink will be returned).
    pub fn freeze<P: AsRef<Path>>(&mut self, path: P) -> io::Result<DirEntry> {
        self.freeze_inner(path.as_ref())
    }

    /// The implementation of [freeze]. The only difference is that this is not generic.
    fn freeze_inner(&mut self, path: &Path) -> io::Result<DirEntry> {
        // freeze() starts at the root of the diagnostic directory and traverses through the path
        // component-by-component. As it does so, it looks for several possible error conditions:
        //
        //   1. A `path` that might be outside the diagnostics directory. Paths that are absolute
        //      (start with a drive prefix or the root directory), paths that contain `..`, and
        //      paths that traverse symlinks might leave the diagnostics directory. For simplicity,
        //      we disallow those path components, as well as `.` for consistency.
        //   2. A `path` that tries to traverse through a file. That is: if names.txt is a normal
        //      file, then names.txt/foo is an invalid path.
        //
        // As it does this scan, it checks `self.frozen` to see if we have already frozen this
        // path. If so, `freeze_inner` can stop traversing path and use the already-frozen
        // `DirEntry` to shortcut its operation. How it does so depends on the `DirEntry`:
        //
        //   3. If the DirEntry is a Dir, then we call `Dir::get_nofollow` on the remaining part of
        //      `path`.
        //   4. If the DirEntry is a File or Symlink, then we verify that `path` does not traverse
        //      through the entry (if so, then we have one of the above two error conditions). If
        //      not, then `path` points at that `DirEntry` and we can just return it.
        //
        // When `path` is exhausted (there are no components left to process), one of two
        // conditions happens:
        //
        //   5. If `path` points to a directory, we call `self.build_dir` which recursively freezes
        //      the directory and builds the Dir.
        //   6. If `path` is a file or symlink, then we call `File::new`/`Symlink::new` to freeze
        //      it and create the corresponding DirEntry.
        //
        // In both cases, the new DirEntry is cached (stored in self.frozen) and returned.

        use ErrorKind::{InvalidInput, NotADirectory, NotFound};
        // The current path, both as an absolute path (for use with OS filesystem calls) and
        // relative to the diagnostics directory (for lookups into self.frozen).
        let mut absolute = self.diagnostics_dir.path().to_path_buf();
        let mut relative = PathBuf::with_capacity(path.as_os_str().len());
        let mut components = path.components();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(ErrorKind::InvalidInput.into());
            };
            absolute.push(name);
            relative.push(name);
            if let Some(entry) = self.frozen.get(&relative) {
                // `path` has already been frozen. This is case 1, 2, 3, or 4.
                if let DirEntry::Dir(dir) = entry {
                    // This is not case 4. Dir::get_nofollow correctly handles cases 1-3.
                    return match dir.get_nofollow(components.as_path()) {
                        Ok(entry) => Ok(entry),
                        Err(GetNofollowError::LeavesDir) => Err(InvalidInput.into()),
                        Err(GetNofollowError::NotADirectory) => Err(NotADirectory.into()),
                        Err(GetNofollowError::NotFound) => Err(NotFound.into()),
                    };
                }
                // This is case 1, 2, or 4. We check whether there are any components left in
                // `path` to determine whether this should error (cases 1 + 2) or succeed (case 4).
                match components.next() {
                    None => return Ok(entry.clone()),
                    Some(_) => return Err(ErrorKind::NotADirectory.into()),
                }
            }
            // Verify that we are not traversing through a file or symlink.
            let metadata = symlink_metadata(&absolute)?;
            let entry_type = metadata.file_type();
            if entry_type.is_dir() {
                continue;
            }
            if components.next().is_some() {
                // Case 1 or 2, return an error
                return Err(NotADirectory.into());
            }
            // `path` points to a file or symlink that has not already been frozen (case 6). Freeze
            // it, store it, and return it.
            let entry = match entry_type.is_symlink() {
                false => DirEntry::File(File::new(
                    self.diagnostics_dir.clone(),
                    absolute,
                    metadata.permissions(),
                )?),
                true => DirEntry::Symlink(Symlink::new(absolute)?),
            };
            self.frozen.insert(relative, entry.clone());
            return Ok(entry);
        }
        // `path` points to a directory (case 5). Recursively freeze that directory, reusing (and
        // removing) any cached sub-entries.
        let entry = DirEntry::Dir(self.build_dir(&mut absolute, &mut relative)?);
        self.frozen.insert(relative.clone(), entry.clone());
        Ok(entry)
    }

    /// Recursive function to freeze and build a Dir. Used by freeze_inner().
    fn build_dir(&mut self, absolute: &mut PathBuf, relative: &mut PathBuf) -> io::Result<Dir> {
        let mut contents = HashMap::new();
        for entry in read_dir(&absolute)? {
            let entry = entry?;
            let file_name = entry.file_name();
            relative.push(&file_name);
            let new_entry = if let Some(entry) = self.frozen.remove(&*relative) {
                entry
            } else {
                absolute.push(&file_name);
                let file_type = entry.file_type()?;
                let entry = match () {
                    _ if file_type.is_file() => File::new(
                        self.diagnostics_dir.clone(),
                        absolute.clone(),
                        entry.metadata()?.permissions(),
                    )?
                    .into(),
                    _ if file_type.is_dir() => self.build_dir(absolute, relative)?.into(),
                    _ => Symlink::new(&absolute)?.into(),
                };
                absolute.pop();
                entry
            };
            contents.insert(file_name, new_entry);
            relative.pop();
        }
        Dir::new(absolute, contents)
    }
}

#[cfg(all(not(miri), test))]
mod tests {
    use super::super::test_util::dir_has_entries;
    use super::*;
    use std::fs::{create_dir, set_permissions, write};
    use std::os::unix::fs::symlink;
    use std::ptr;

    #[test]
    fn freeze() {
        use ErrorKind::{InvalidInput, NotADirectory, NotFound};
        let diagnostics_dir = Arc::new(DiagnosticsDir::tempdir().unwrap());
        let mut freezer = Freezer::new(diagnostics_dir.clone());
        // Build the following filesystem structure under the diagnostics directory:
        //
        // a/outer_file       A basic file
        // a/b/absolute_link  An absolute symlink (should not be followed).
        // a/b/c              A subdirectory.
        // a/b/inner_file     A basic file
        // a/b/dir_link       A symlink to a/
        // a/b/file_link      A symlink to a/outer_file
        let a = PathBuf::from_iter([diagnostics_dir.path(), "a".as_ref()]);
        let a_b = PathBuf::from_iter([a.as_path(), "b".as_ref()]);
        let a_b_absolute_link = PathBuf::from_iter([a_b.as_path(), "absolute_link".as_ref()]);
        let a_b_c = PathBuf::from_iter([a_b.as_path(), "c".as_ref()]);
        let a_b_inner_file = PathBuf::from_iter([a_b.as_path(), "inner_file".as_ref()]);
        let a_b_dir_link = PathBuf::from_iter([a_b.as_path(), "dir_link".as_ref()]);
        let a_b_file_link = PathBuf::from_iter([a_b.as_path(), "file_link".as_ref()]);
        let a_outer_file = PathBuf::from_iter([a.as_path(), "outer_file".as_ref()]);
        create_dir(&a).unwrap();
        write(&a_outer_file, "outer_file").unwrap();
        create_dir(&a_b).unwrap();
        symlink("/absolute", a_b_absolute_link).unwrap();
        create_dir(&a_b_c).unwrap();
        write(&a_b_inner_file, "").unwrap();
        symlink("../..", a_b_dir_link).unwrap();
        symlink("../outer_file", a_b_file_link).unwrap();
        // Try freezing a few invalid paths first.
        let error = freezer.freeze("/absolute").unwrap_err();
        assert_eq!(error.kind(), InvalidInput);
        assert_eq!(freezer.freeze("./a").unwrap_err().kind(), InvalidInput);
        let error = freezer.freeze("a/outer_file/c").unwrap_err();
        assert_eq!(error.kind(), NotADirectory);
        let error = freezer.freeze("a/b/dir_link/b").unwrap_err();
        assert_eq!(error.kind(), NotADirectory);
        assert_eq!(freezer.freeze("nonexistent").unwrap_err().kind(), NotFound);
        // Freeze a/b/inner_file
        let entry = freezer.freeze("a/b/inner_file").unwrap();
        assert!(entry.file().is_some());
        let is_readonly = |path| symlink_metadata(path).unwrap().permissions().readonly();
        assert!(!is_readonly(&a_b));
        let mut inner_file_perms = symlink_metadata(&a_b_inner_file).unwrap().permissions();
        assert!(inner_file_perms.readonly());
        // Freeze a/b/dir_link
        let a_b_dir_link_symlink = freezer.freeze("a/b/dir_link").unwrap().symlink().unwrap();
        assert_eq!(a_b_dir_link_symlink.contents(), "../..");
        assert!(!is_readonly(&a_b));
        // Freezer should not re-freeze a/b/inner_file and a/b/dir_link when a/b is frozen.
        // However, there's not a great way to verify that. What we do here is:
        // 1. For a/b/inner_file, we make the file writable again, then verify that it is still
        //    writable after freezing a/b.
        // 2. For a/b/dir_link, we verify the returned path has the same address as the first copy.
        inner_file_perms.set_readonly(false);
        set_permissions(&a_b_inner_file, inner_file_perms).unwrap();
        // Freeze a/b
        let a_b_dir = freezer.freeze("a/b").unwrap().dir().unwrap();
        let entry = |n| a_b_dir.get_nofollow(n).unwrap();
        let absolute_link_symlink = entry("absolute_link").symlink().unwrap();
        assert_eq!(absolute_link_symlink.contents(), "/absolute");
        assert_eq!(entry("c").dir().unwrap().entries().count(), 0);
        assert_eq!(entry("inner_file").file().is_some(), true);
        let a_b_dir_link_symlink_2 = entry("dir_link").symlink().unwrap();
        assert_eq!(a_b_dir_link_symlink_2.contents(), "../..");
        let file_link_symlink = entry("file_link").symlink().unwrap();
        assert_eq!(file_link_symlink.contents(), "../outer_file");
        assert_eq!(a_b_dir.entries().count(), 5);
        // Verify that everything has the expected permissions.
        assert!(!is_readonly(&a));
        let mut a_b_perms = symlink_metadata(&a_b).unwrap().permissions();
        assert!(a_b_perms.readonly());
        let mut a_b_c_perms = symlink_metadata(&a_b_c).unwrap().permissions();
        assert!(a_b_c_perms.readonly());
        assert!(!is_readonly(&a_b_inner_file)); // Verifies a/b/inner_file not re-frozen
        assert!(!is_readonly(&a_outer_file));
        // Check that a/b/dir_link has the same path.
        assert!(ptr::eq(
            a_b_dir_link_symlink.contents(),
            a_b_dir_link_symlink_2.contents()
        ));
        // Repeat the previous readonly trick to verify that freezing a/ does not re-freeze
        // anything under b/.
        a_b_perms.set_readonly(false);
        a_b_c_perms.set_readonly(false);
        set_permissions(&a_b, a_b_perms).unwrap();
        set_permissions(&a_b_c, a_b_c_perms).unwrap();
        // Freeze a/
        let a_dir = freezer.freeze("a").unwrap().dir().unwrap();
        let a_b_dir_2 = a_dir.get_nofollow("b").unwrap().dir().unwrap();
        assert!(a_dir.get_nofollow("outer_file").unwrap().file().is_some());
        assert_eq!(a_dir.entries().count(), 2);
        assert!(dir_has_entries(
            &a_b_dir_2,
            &["absolute_link", "c", "dir_link", "file_link", "inner_file"]
        ));
        assert!(is_readonly(&a));
        assert!(!is_readonly(&a_b)); // Should not re-freeze
        assert!(!is_readonly(&a_b_c)); // Should not re-freeze
        assert!(!is_readonly(&a_b_inner_file)); // Should not re-freeze
        assert!(is_readonly(&a_outer_file));
        // Verify that the nested frozen paths were removed.
        assert_eq!(freezer.frozen.len(), 1);
        assert_eq!(freezer.frozen.iter().next().map(|(n, _)| n).unwrap(), "a");
    }
}
