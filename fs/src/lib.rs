/// Filesystem abstractions. Used to represent directory trees, such as the input project or
/// lowered Rust source.
///
/// These types are read-only. To create them:
/// 1. Create the file/directory/symlink in the diagnostic directory and populate it as intended.
/// 2. "freeze" the file using `Reporter::freeze_path` or (TODO: what ToolReporter function or
///    related thing do tools use?).
///
/// Freezing the contents will:
/// 1. Make the on-disk structures read-only. This applies recursively, but does not follow
///    symlinks.
/// 2. Construct a `DirEntry` representing the on-disk structure.
///
/// After you have frozen a filesystem object, it (and everything else frozen with it, if it is a
/// directory) must be left unchanged in the diagnostic directory. This is to avoid the need to
/// store the contents of files in memory.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

/// View of a read-only directory element.
#[derive(Clone)]
pub enum DirEntry {
    Dir(Dir),
    File(File),
    Symlink(Symlink),
}

// Note: File and TextFile are internally Arc<> to a single shared type. That way, the UTF-8-ness
// of the file can be shared between the copies, because it is computed lazily.
/// A read-only file.
#[derive(Clone)]
pub struct File {}
/// A read-only UTF-8 file.
#[derive(Clone)]
pub struct TextFile {}

/// A frozen directory.
#[derive(Clone)]
pub struct Dir {
    contents: Arc<HashMap<OsString, DirEntry>>,
}

impl Dir {
    /// Retrieves the entry at the specified location. If you want a recursive lookup (traversing
    /// into subdirectories), use [get_recursive] instead.
    /// Returns `None` if there is no entry at `name`.
    pub fn get<N: AsRef<OsStr>>(&self, name: N) -> Option<DirEntry> {
        self.contents.get(name.as_ref()).cloned()
    }

    /// Retrieves the entry at the specified location under this directory. This will resolve
    /// symlinks, but only if they are relative and point to paths that lie within this directory
    /// (as otherwise this `Dir` does not have enough context to follow them).
    // TODO: Unit test
    pub fn get_recursive<P: AsRef<Path>>(&self, path: P) -> Result<DirEntry, GetRecursiveError> {
        let _ = path;
        todo!()
    }

    /// Iterates through the contents of this directory.
    pub fn entries(&self) -> impl Iterator<Item = (OsString, DirEntry)> {
        self.contents.iter().map(|(p, e)| (p.clone(), e.clone()))
    }
}

#[derive(Debug, Error)]
pub enum GetRecursiveError {
}

/// A symlink that has been frozen. Note that the thing it points to is not frozen; in fact it may
/// not exist or may be entirely outside the diagnostics directory.
#[derive(Clone)]
pub struct Symlink {
    // The path contained by this symlink.
    contents: PathBuf,
}

impl Symlink {
    pub fn contents(&self) -> &Path {
        &self.contents
    }

    /// Writes this symlink into the filesystem at the given path.
    pub fn write_rw<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        symlink(&self.contents, path)
    }
}
