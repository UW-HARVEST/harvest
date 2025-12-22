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

use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
use tempfile::TempDir;

pub use dir::{Dir, GetError};
pub use file::{File, TextFile};

/// Owns the diagnostics directory (if it is a temporary directory) and stores useful information
/// about it.
#[allow(dead_code)] // TODO: Remove once this is moved into harvest_ir/harvest_core.
#[derive(Debug)]
pub struct DiagnosticsDir {
    path: PathBuf,
    // Initialized to false; set to true if a reflink copy fails. Allows files to skip trying
    // reflink copies if they're never going to succeed.
    reflink_failed: AtomicBool,
    // Owns the directory if it is temporary, otherwise is None.
    _tempdir: Option<TempDir>,
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

/// A DirEntry after symlinks have been fully resolved.
#[derive(Clone, Debug)]
pub enum ResolvedEntry {
    Dir(Dir),
    File(File),
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
    pub fn contents(&self) -> &Path {
        &self.contents
    }

    /// Writes this symlink into the filesystem at the given path.
    pub fn write_rw<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        symlink(&self.contents, path)
    }
}
