#[cfg(all(not(miri), test))]
mod tests;

use super::{File, Symlink};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::Permissions;
use std::fs::set_permissions;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Component, Path};
use std::sync::Arc;
use thiserror::Error;

/// View of a frozen directory element.
#[derive(Clone, Debug)]
pub enum DirEntry {
    Dir(Dir),
    File(File),
    Symlink(Symlink),
}

impl DirEntry {
    /// Returns the contained [Dir] if this is a directory.
    pub fn dir(&self) -> Option<Dir> {
        match self {
            DirEntry::Dir(dir) => Some(dir.clone()),
            _ => None,
        }
    }

    /// Returns the contained [File] if this is a file.
    pub fn file(&self) -> Option<File> {
        match self {
            DirEntry::File(file) => Some(file.clone()),
            _ => None,
        }
    }

    /// Returns the contained [Symlink] if this is a symlink.
    pub fn symlink(&self) -> Option<Symlink> {
        match self {
            DirEntry::Symlink(symlink) => Some(symlink.clone()),
            _ => None,
        }
    }
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

/// A DirEntry after symlinks have been fully resolved.
#[derive(Clone, Debug)]
pub enum ResolvedEntry {
    Dir(Dir),
    File(File),
}

impl ResolvedEntry {
    /// Returns the contained [Dir] if this is a directory.
    pub fn dir(&self) -> Option<Dir> {
        match self {
            ResolvedEntry::Dir(dir) => Some(dir.clone()),
            _ => None,
        }
    }

    /// Returns the contained [File] if this is a file.
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

/// A frozen directory.
#[derive(Clone, Debug)]
pub struct Dir {
    contents: Arc<HashMap<OsString, DirEntry>>,
}

impl Dir {
    /// Creates a new Dir with the given contents. For internal use by `Freezer::freeze`.
    pub(super) fn new(absolute: &Path, contents: HashMap<OsString, DirEntry>) -> io::Result<Dir> {
        // Readable and executable (execute on a directory means you can traverse it)
        set_permissions(absolute, Permissions::from_mode(0o500))?;
        Ok(Dir {
            contents: Arc::new(contents),
        })
    }

    /// Iterates through the contents of this directory.
    pub fn entries(&self) -> impl Iterator<Item = (OsString, DirEntry)> {
        self.contents.iter().map(|(p, e)| (p.clone(), e.clone()))
    }

    /// Retrieves the entry at the specified location under this directory. This will resolve
    /// symlinks, but only if they are relative and do not traverse outside this `Dir`.
    pub fn get<P: AsRef<Path>>(&self, path: P) -> Result<ResolvedEntry, GetError> {
        let _ = path;
        todo!()
    }

    /// Retrieves the entry at the specified location. If you want a recursive lookup (traversing
    /// into subdirectories), use [Dir::get] instead.
    /// Returns `None` if there is no entry at `name`.
    pub fn get_entry<N: AsRef<OsStr>>(&self, name: N) -> Option<DirEntry> {
        self.contents.get(name.as_ref()).cloned()
    }

    /// Retrieves the entry at the specified location under this directory without following
    /// symlinks or `.`/`..` entries. If an intermediate directory is a symlink (e.g. the path is
    /// `a/b/c` where `a/b` is a symlink), this will return NotADirectory.
    pub fn get_nofollow<P: AsRef<Path>>(&self, path: P) -> Result<DirEntry, GetNofollowError> {
        self.get_nofollow_inner(path.as_ref())
    }

    /// The implementation of [get_nofollow]. The only difference is this is not generic.
    pub fn get_nofollow_inner(&self, path: &Path) -> Result<DirEntry, GetNofollowError> {
        use Component::*;
        let mut components = path.components();
        let mut cur_dir = self;
        while let Some(component) = components.next() {
            let name = match component {
                Prefix(_) | RootDir => return Err(GetNofollowError::LeavesDir),
                CurDir | ParentDir => return Err(GetNofollowError::NotADirectory),
                Normal(name) => name,
            };
            match cur_dir.contents.get(name) {
                None => return Err(GetNofollowError::NotFound),
                Some(DirEntry::Dir(new_dir)) => cur_dir = new_dir,
                Some(entry) => match components.next() {
                    None => return Ok(entry.clone()),
                    Some(_) => return Err(GetNofollowError::NotADirectory),
                },
            }
        }
        Ok(DirEntry::Dir(cur_dir.clone()))
    }
}

/// An error returned from [Dir::get].
#[derive(Debug, Error, Hash, Eq, PartialEq)]
pub enum GetError {
    #[error("symlink loop")]
    FilesystemLoop,
    #[error("path leaves the Dir")]
    LeavesDir,
    #[error("intermediate path component is a file")]
    NotADirectory,
    #[error("file or directory not found")]
    NotFound,
}

/// An error returned from [Dir::get_nofollow].
#[derive(Debug, Error, Hash, Eq, PartialEq)]
pub enum GetNofollowError {
    #[error("path leaves the Dir")]
    LeavesDir,
    #[error("intermediate path component is a file")]
    NotADirectory,
    #[error("file or directory not found")]
    NotFound,
}
