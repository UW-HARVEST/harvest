/// Filesystem abstractions. Used to represent directory trees, such as the input project or
/// lowered Rust source.
///
/// # Freezing
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
///
/// # Symlinks
///
/// Symlink resolution is always performed in the context of a particular `Dir`, and acts as if
/// that particular `Dir` is floating in space (i.e., it does not know the `Dir`'s parent or its
/// location relative to the filesystem root). As a result, absolute symlinks cannot be followed.
/// Further, it means that whether or not a symlink is resolvable (and what it resolves to) can
/// depend on which `Dir` you use to query a symlink.
///
/// For example, suppose you create the following directory structure then freeze it (and call the
/// frozen directory `a`):
///
/// ```
/// $ mkdir c
/// $ ln -s '../b' c/d
/// $ ls -l . c/
/// .:
/// total 4
/// -rw-rw-r-- 1 ryan ryan    0 Dec 17 16:05 b
/// drwxrwxr-x 2 ryan ryan 4096 Dec 17 16:05 c
/// 
/// c/:
/// total 0
/// lrwxrwxrwx 1 ryan ryan 4 Dec 17 16:05 d -> ../b
/// ```
///
/// If you resolve `b/d` from the context of `a/`, then it will resolve to `c`. But if you instead
/// retrieve the `b/` `Dir` and try to resolve `d` from them, resolution will fail (because the
/// resolution traverses outside `b/`).

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Component, Path};
use std::sync::Arc;
use thiserror::Error;

/// View of a read-only directory element.
#[derive(Clone)]
pub enum DirEntry {
    Dir(Dir),
    File(File),
    Symlink(Symlink),
}

/// A DirEntry after symlinks have been fully resolved.
#[derive(Clone)]
pub enum ResolvedEntry {
    Dir(Dir),
    File(File),
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
    /// symlinks, but only if they are relative and do not traverse outside this `Dir`.
    // TODO: Unit test
    pub fn get_recursive<P: AsRef<Path>>(&self, path: P) -> Result<ResolvedEntry, GetRecursiveError> {
        use GetRecursiveError::*;

        /// By-reference variant of ResolvedEntry
        enum ResolvedRef<'s> {
            Dir(&'s Dir),
            File(&'s File),
        }

        //// Symlink-resolution cache. Used to discover symlink cycles and avoid exponential lookup
        //// behavior.
        //// When this function first tries to resolve a symlink, it inserts a `None` for that
        //// symlink into the cache. Once the symlink has been fully resolved, the entry is changed
        //// to `Some`. Therefore, if a `None` cache entry is discovered during symlink resolution,
        //// that symlink is cyclic (it depends on itself, either directly or indirectly).
        //// The key of the map is the address of the Symlink, the value is a tuple containing:
        //// 1. A Vec<&Dir>, containing all directories between *self and the symlink's target.
        //// 2. A &DirEntry pointing to the symlink's target.
        //let mut symlink_cache = HashMap::new();
        // All directories from *self (not inclusive) to the current directory (inclusive). If the
        // current directory is *self, this is empty.
        let mut parents = vec![];
        //// Resolves a symlink.
        //fn resolve_symlink(cache: &mut HashMap<usize, (Vec<&Dir>, &DirEntry)>, parents: &mut Vec<&Dir>) -> 
        let mut components = path.as_ref().components();
        for component in &mut components {
            // Handle non-normal components first.
            let name = match component {
                Component::Prefix(_) | Component::RootDir => return Err(LeavesDir),
                Component::CurDir => continue,
                Component::ParentDir => match parents.pop() {
                    None => return Err(LeavesDir),
                    Some(_) => continue,
                },
                Component::Normal(name) => name,
            };
            let dir_entry = self.contents.get(name).ok_or(NotFound)?;
            // Resolve symlinks
            let resolved = match dir_entry {
                DirEntry::Dir(dir) => ResolvedRef::Dir(dir),
                DirEntry::File(file) => ResolvedRef::File(file),
                DirEntry::Symlink(symlink) => todo!(),
            };
            // If a file was found, verify this was the end of the path.
            match resolved {
                ResolvedRef::Dir(dir) => parents.push(dir),
                ResolvedRef::File(_) if components.next().is_some() => return Err(NotADirectory),
                ResolvedRef::File(file) => return Ok(ResolvedEntry::File(file.clone())),
            };
        }
        todo!()
    }

    /// Iterates through the contents of this directory.
    pub fn entries(&self) -> impl Iterator<Item = (OsString, DirEntry)> {
        self.contents.iter().map(|(p, e)| (p.clone(), e.clone()))
    }
}

#[derive(Debug, Error)]
pub enum GetRecursiveError {
    #[error("path leaves the Dir")]
    LeavesDir,
    #[error("intermediate path component is a file")]
    NotADirectory,
    #[error("file or directory not found")]
    NotFound,
}

/// A symlink that has been frozen. Note that the thing it points to is not frozen; in fact it may
/// not exist or may be entirely outside the diagnostics directory.
#[derive(Clone)]
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
