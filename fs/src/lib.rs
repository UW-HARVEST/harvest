//! Filesystem abstractions. Used to represent directory trees, such as the input project or
//! lowered Rust source.
//!
//! # Freezing
//!
//! These types are read-only. To create them:
//! 1. Create the file/directory/symlink in the diagnostic directory and populate it as intended.
//! 2. "freeze" the file using `Reporter::freeze_path` or (TODO: what ToolReporter function or
//!    related thing do tools use?).
//!
//! Freezing the contents will:
//! 1. Make the on-disk structures read-only. This applies recursively, but does not follow
//!    symlinks.
//! 2. Construct a `DirEntry` representing the on-disk structure.
//!
//! After you have frozen a filesystem object, it (and everything else frozen with it, if it is a
//! directory) must be left unchanged in the diagnostic directory. This is to avoid the need to
//! store the contents of files in memory.
//!
//! # Symlinks
//!
//! Symlink resolution is always performed in the context of a particular `Dir`, and acts as if
//! that particular `Dir` is floating in space (i.e., it does not know the `Dir`'s parent or its
//! location relative to the filesystem root). As a result, absolute symlinks cannot be followed.
//! Further, it means that whether or not a symlink is resolvable (and what it resolves to) can
//! depend on which `Dir` you use to query a symlink.
//!
//! For example, suppose you create the following directory structure then freeze it (and call the
//! frozen directory `a`):
//!
//! ```shell
//! $ mkdir c
//! $ ln -s '../b' c/d
//! $ ls -l . c/
//! .:
//! total 4
//! -rw-rw-r-- 1 ryan ryan    0 Dec 17 16:05 b
//! drwxrwxr-x 2 ryan ryan 4096 Dec 17 16:05 c
//!
//! c/:
//! total 0
//! lrwxrwxrwx 1 ryan ryan 4 Dec 17 16:05 d -> ../b
//! ```
//!
//! If you resolve `b/d` from the context of `a/`, then it will resolve to `c`. But if you instead
//! retrieve the `b/` `Dir` and try to resolve `d` from them, resolution will fail (because the
//! resolution traverses outside `b/`).

mod dir;
mod file;

use std::collections::HashMap;
use std::fs::{Permissions, metadata, read_dir, read_link, remove_file, set_permissions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use tempfile::TempDir;

pub use dir::{Dir, GetError, GetNofollowError};
pub use file::{File, TextFile};

/// Utility to recursively delete a TempDir that contains read-only files and directories. Provided
/// to make it easier to delete the diagnostics directory (note that [DiagnosticsDir] automatically
/// deletes the diagnostic directory on drop if it is a TempDir).
pub fn delete_ro_tempdir(tempdir: TempDir) -> io::Result<()> {
    fn delete_contents(path: &mut PathBuf) -> io::Result<()> {
        set_permissions(&path, Permissions::from_mode(0o700))?;
        for entry in read_dir(&path)? {
            let entry = entry?;
            path.push(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                delete_contents(path)?;
            } else {
                if !file_type.is_symlink() {
                    set_permissions(&path, Permissions::from_mode(0o200))?;
                }
                remove_file(&path)?;
            }
            path.pop();
        }
        Ok(())
    }
    let mut path = tempdir.path().into();
    delete_contents(&mut path)?;
    tempdir.close()
}

/// Owns the diagnostics directory (if it is a temporary directory) and stores useful information
/// about it.
#[derive(Debug)]
pub struct DiagnosticsDir {
    path: PathBuf,
    // Initialized to false; set to true if a reflink copy fails. Allows files to skip trying
    // reflink copies if they're never going to succeed.
    #[allow(dead_code)] // TODO: Remove
    reflink_failed: AtomicBool,
    // Owns the directory if it is temporary, otherwise is None.
    tempdir: Option<TempDir>,
}

impl Drop for DiagnosticsDir {
    fn drop(&mut self) {
        self.tempdir.take().map(|d| delete_ro_tempdir(d).unwrap());
    }
}

/// View of a read-only directory element.
#[derive(Clone, Debug)]
pub enum DirEntry {
    Dir(Dir),
    File(File),
    Symlink(Symlink),
}

impl From<Dir> for DirEntry {
    fn from(dir: Dir) -> DirEntry {
        DirEntry::Dir(dir)
    }
}

impl From<File> for DirEntry {
    fn from(file: File) -> DirEntry {
        DirEntry::File(file)
    }
}

impl From<ResolvedEntry> for DirEntry {
    fn from(resolved: ResolvedEntry) -> DirEntry {
        match resolved {
            ResolvedEntry::Dir(dir) => DirEntry::Dir(dir),
            ResolvedEntry::File(file) => DirEntry::File(file),
        }
    }
}

impl From<Symlink> for DirEntry {
    fn from(symlink: Symlink) -> DirEntry {
        DirEntry::Symlink(symlink)
    }
}

// This should be removed when this is migrated into harvest_core and the functionality put into
// Reporter instead.
pub struct Freezer {
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

    /// Freezes the given path, returning an object referencing it. `path` must be relative to the
    /// diagnostics directory, and cannot contain `.` or `..`. This will not follow symlinks (i.e.
    /// `path` cannot have symlinks in its directory path, and if `path` points to a symlink then a
    /// Symlink will be returned).
    pub fn freeze<P: AsRef<Path>>(&mut self, path: P) -> io::Result<DirEntry> {
        self.freeze_inner(path.as_ref())
    }

    /// The implementation of [freeze]. The only difference is that this is not generic.
    fn freeze_inner(&mut self, path: &Path) -> io::Result<DirEntry> {
        use ErrorKind::{InvalidInput, NotADirectory, NotFound};
        // The current path, both as an absolute path and relative to the diagnostics directory.
        let mut absolute = self.diagnostics_dir.path.clone();
        let mut relative = PathBuf::with_capacity(path.as_os_str().len());
        let mut components = path.components();
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(ErrorKind::InvalidInput.into());
            };
            absolute.push(name);
            relative.push(name);
            if let Some(entry) = self.frozen.get(&relative) {
                if let DirEntry::Dir(dir) = entry {
                    return match dir.get_nofollow(components.as_path()) {
                        Ok(entry) => Ok(entry),
                        Err(GetNofollowError::LeavesDir) => Err(InvalidInput.into()),
                        Err(GetNofollowError::NotADirectory) => Err(NotADirectory.into()),
                        Err(GetNofollowError::NotFound) => Err(NotFound.into()),
                    };
                }
                // We don't descend through symlinks (and cannot descend through files). If we
                // encounter one, we check whether the remaining path is empty to determine whether
                // to return an error or the entry.
                match components.next() {
                    None => return Ok(entry.clone()),
                    Some(_) => return Err(ErrorKind::NotADirectory.into()),
                }
            }
            // Verify that we are not traversing through a file or symlink.
            let entry_type = metadata(&absolute)?.file_type();
            if entry_type.is_dir() {
                continue;
            }
            if components.next().is_some() {
                return Err(NotADirectory.into());
            }
            // `path` points to a file or symlink that has not already been frozen. Freeze it,
            // store it, and return it.
            let entry = match entry_type.is_symlink() {
                false => DirEntry::File(File::new(self.diagnostics_dir.clone(), absolute)?),
                true => DirEntry::Symlink(Symlink::new(absolute)?),
            };
            self.frozen.insert(relative, entry.clone());
            return Ok(entry);
        }
        // `path` points to a directory. Recursively freeze that directory, reusing (and removing)
        // any cached sub-entries.
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
                let entry = match file_type.is_file() {
                    true => File::new(self.diagnostics_dir.clone(), relative.clone())?.into(),
                    _ if file_type.is_dir() => self.build_dir(absolute, relative)?.into(),
                    _ => Symlink::new(&absolute)?.into(),
                };
                absolute.pop();
                entry
            };
            contents.insert(file_name, new_entry);
            relative.pop();
        }
        Dir::new(&absolute, contents)
    }
}

/// A DirEntry after symlinks have been fully resolved.
#[derive(Clone, Debug)]
pub enum ResolvedEntry {
    Dir(Dir),
    File(File),
}

impl ResolvedEntry {
    pub fn dir(&self) -> Option<Dir> {
        match self {
            ResolvedEntry::Dir(dir) => Some(dir.clone()),
            _ => None,
        }
    }

    pub fn file(&self) -> Option<File> {
        match self {
            ResolvedEntry::File(file) => Some(file.clone()),
            _ => None,
        }
    }
}

impl From<Dir> for ResolvedEntry {
    fn from(dir: Dir) -> ResolvedEntry {
        ResolvedEntry::Dir(dir)
    }
}

impl From<File> for ResolvedEntry {
    fn from(file: File) -> ResolvedEntry {
        ResolvedEntry::File(file)
    }
}

/// A symlink that has been frozen. Note that the thing it points to is not frozen; in fact it may
/// not exist or may be entirely outside the diagnostics directory.
#[derive(Clone, Debug)]
pub struct Symlink {
    // The path contained by this symlink.
    contents: Arc<Path>,
}

impl Symlink {
    /// Creates a new Symlink representing the file at the given path. This is for internal use by
    /// the diagnostics system; tool code should use [Reporter::freeze] or /* TODO: what
    /// ToolReporter function? */ to create a symlink.
    fn new<P: AsRef<Path>>(path: P) -> io::Result<Symlink> {
        // Symlink permissions cannot be changed, so just create the Symlink object.
        Ok(Symlink {
            contents: read_link(path)?.into(),
        })
    }

    pub fn contents(&self) -> &Path {
        &self.contents
    }

    /// Writes this symlink into the filesystem at the given path.
    pub fn write_rw<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        symlink(&self.contents, path)
    }
}
