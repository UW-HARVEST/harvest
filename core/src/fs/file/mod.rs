#[cfg(all(not(miri), test))]
mod tests;

use super::DiagnosticsDir;
use std::fs::{Permissions, read, read_to_string, set_permissions};
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::str::{Utf8Error, from_utf8};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tracing::error;

// Note: File and TextFile are internally Arc<> to a single shared type. That way, the UTF-8-ness
// of the file can be shared between the copies, because it is computed lazily.
/// A frozen file. A file can be a valid UTF-8, in which case it is considered a text file, or not
/// UTF-8, in which case it is not. A [File] can be converted into a [TextFile] using [TryFrom] if
/// the file is valid UTF-8.
#[derive(Clone, Debug)]
pub struct File {
    shared: Arc<Shared>,
}

impl File {
    /// Freezes the given file and returns a new File object referring to it. Note that this is for
    /// internal use by the diagnostics system; other code should use `Reporter::freeze` to create
    /// a File.
    pub(super) fn new(
        diagnostics_dir: Arc<DiagnosticsDir>,
        absolute: PathBuf,
        relative: PathBuf,
        permissions: Permissions,
    ) -> io::Result<File> {
        // Remove write permissions from user and all permissions from group + other. Leave execute
        // permission for user as-is.
        set_permissions(
            &absolute,
            Permissions::from_mode(permissions.mode() & 0o500),
        )?;
        let shared = Arc::new(Shared {
            contents: Mutex::new(CachedContents::Unknown),
            _diagnostics_dir: diagnostics_dir,
            absolute,
            relative,
        });
        Ok(File { shared })
    }

    /// Returns this file's contents as a byte array.
    pub fn bytes(&self) -> Arc<[u8]> {
        match self.shared.contents() {
            Contents::Utf8(contents) => contents.into(),
            Contents::NotUtf8 { contents, .. } => contents,
        }
    }

    /// Returns true if this file is UTF-8 (in which case it can be converted into a TextFile),
    /// false otherwise.
    pub fn is_utf8(&self) -> bool {
        <TextFile as TryFrom<_>>::try_from(self.clone()).is_ok()
    }

    /// Returns the path to this file (or one instance thereof) in the diagnostic directory.
    pub fn path(&self) -> &Path {
        &self.shared.absolute
    }

    /// Used by Freezer::copy_ro. Writes a read-only copy of this File into the given path. Note
    /// that `relative` must consist only of Normal components and may not traverse symlinks.
    pub(super) fn copy_ro(&self, absolute: &Path, relative: &Path) -> io::Result<()> {
        // Compute the relative path from `relative` to `self.shared.relative`. This path will be
        // the symlink's contents. For example, suppose that:
        //
        //   target = self.shared.relative = a/b/c/d/e
        //   source = relative             = a/b/w/y
        //
        // First, you can strip off the leading components that are the same:
        //
        //   target = c/d/e
        //   source = w/y
        //
        // You can then construct the symlink path as repeat(..)/target, where the number of ..'s
        // is one fewer than the number of remaining components in `source`. Note that this assumes
        // that all remaining components of `source` are Normal components, which the caller must
        // guarantee.
        let mut source_components = relative.components();
        let mut target_components = self.shared.relative.components();
        // Iterate until we find the first differing component between source and target.
        let target_component = loop {
            let target_component = target_components.next().expect("path traverses a file");
            if source_components.next().expect("path is a dir") != target_component {
                break target_component;
            }
        };
        // At this point, we've iterated past all of the common components of source and target and
        // have popped the first distinct components from each. Therefore the number of remaining
        // components in source_components is one fewer than the number of distinct components from
        // source, which matches the number of ..'s to push into the symlink path.
        let mut symlink_path = PathBuf::new();
        symlink_path.extend(source_components.map(|_| ".."));
        // Add the target component we've already popped, and the remainder of target_components.
        symlink_path.push(target_component);
        symlink_path.push(target_components.as_path());
        // Write the symlink into the filesystem.
        symlink(symlink_path, absolute)
    }
}

impl From<TextFile> for File {
    fn from(file: TextFile) -> File {
        File {
            shared: file.shared.clone(),
        }
    }
}

/// A frozen UTF-8 file.
#[derive(Clone, Debug)]
pub struct TextFile {
    // Invariant: shared.contents is a Contents::Utf8().
    shared: Arc<Shared>,
}

impl TextFile {
    /// Returns this file's contents as a byte array.
    pub fn bytes(&self) -> Arc<[u8]> {
        self.str().into()
    }

    /// Returns the path to this file (or one instance thereof) in the diagnostic directory.
    pub fn path(&self) -> &Path {
        &self.shared.absolute
    }

    /// Returns this file's contents as a str.
    pub fn str(&self) -> Arc<str> {
        match self.shared.contents() {
            Contents::Utf8(contents) => contents,
            _ => panic!("non-UTF-8 TextFile"),
        }
    }
}

impl TryFrom<File> for TextFile {
    type Error = Utf8Error;
    /// Checks if this file is valid UTF-8, and converts it into a Textfile if it is.
    fn try_from(file: File) -> Result<TextFile, Utf8Error> {
        let guard = file.shared.lock_contents();
        let make_textfile = || TextFile {
            shared: file.shared.clone(),
        };
        // If this file has already been loaded, then the cache will tell us whether it is UTF-8.
        // Check the cache first.
        match *guard {
            CachedContents::Unknown => {}
            CachedContents::Utf8(_) => return Ok(make_textfile()),
            CachedContents::NotUtf8 { error, .. } => return Err(error),
        }
        // The file has not been loaded before, so load it to check if it is UTF-8. load will
        // populate the cache so we won't have to load this file again.
        match file.shared.load(guard) {
            Contents::Utf8(_) => Ok(make_textfile()),
            Contents::NotUtf8 { error, .. } => Err(error),
        }
    }
}

/// Data for [File]s and [TextFile]s.
#[derive(Debug)]
struct Shared {
    contents: Mutex<CachedContents>,

    // Absolute path to a copy of this file in the filesystem.
    absolute: PathBuf,
    // Direct path to a copy of this file in the filesystem, relative to the diagnostic directory.
    relative: PathBuf,

    // Handle to the DiagnosticsDir so the backing file is not deleted.
    _diagnostics_dir: Arc<DiagnosticsDir>,
}

impl Shared {
    /// Returns this file's contents as a [Contents]. This will check the cache first, then load
    /// the file from the filesystem if necessary.
    fn contents(&self) -> Contents {
        let mut guard = self.lock_contents();
        match *guard {
            CachedContents::Unknown => self.load(guard),
            CachedContents::Utf8(ref contents) => match contents.upgrade() {
                None => {
                    // We know that this file is valid UTF-8, but its contents have been forgotten
                    // (all Arc<>s dropped). Read it in again and re-cache its contents.
                    let contents = read_to_string(&self.absolute)
                        .expect("read of frozen text file failed")
                        .into();
                    *guard = CachedContents::Utf8(Arc::downgrade(&contents));
                    Contents::Utf8(contents)
                }
                Some(contents) => Contents::Utf8(contents),
            },
            CachedContents::NotUtf8 {
                ref contents,
                error,
            } => match contents.upgrade() {
                None => {
                    // We know that this file is not valid UTF-8, but its contents have been
                    // forgotten (all Arc<>s dropped). Read it in again and re-cache its contents.
                    let contents = read(&self.absolute)
                        .expect("read of frozen file failed")
                        .into();
                    *guard = CachedContents::NotUtf8 {
                        contents: Arc::downgrade(&contents),
                        error,
                    };
                    Contents::NotUtf8 { contents, error }
                }
                Some(contents) => Contents::NotUtf8 { contents, error },
            },
        }
    }

    /// Reads in the file contents, updating the cache and returning them.
    fn load(&self, mut guard: MutexGuard<CachedContents>) -> Contents {
        let contents = read(&self.absolute).expect("read of frozen file failed");
        match from_utf8(&contents) {
            Ok(contents) => {
                let contents = Arc::from(contents);
                *guard = CachedContents::Utf8(Arc::downgrade(&contents));
                Contents::Utf8(contents)
            }
            Err(error) => {
                let contents = Arc::from(contents);
                *guard = CachedContents::NotUtf8 {
                    contents: Arc::downgrade(&contents),
                    error,
                };
                Contents::NotUtf8 { contents, error }
            }
        }
    }

    /// Locks self.contents, returning the guard.
    fn lock_contents(&self) -> MutexGuard<'_, CachedContents> {
        self.contents.lock().unwrap_or_else(|e| {
            error!("file::Shared contents poisoned");
            self.contents.clear_poison();
            e.into_inner()
        })
    }
}

/// This file's contents. Returned by [Shared::contents]
#[derive(Debug)]
enum Contents {
    /// This file is UTF-8.
    Utf8(Arc<str>),
    /// This file is not UTF-8.
    NotUtf8 {
        contents: Arc<[u8]>,
        error: Utf8Error,
    },
}

/// Cached data about this file's contents. The cache remembers whether the file is UTF-8 forever,
/// but only keeps weak pointers to the file's data.
#[derive(Debug)]
enum CachedContents {
    /// This file has never been loaded so we're unaware of the contents.
    Unknown,
    /// This file is UTF-8.
    Utf8(Weak<str>),
    /// This file is not UTF-8.
    NotUtf8 {
        contents: Weak<[u8]>,
        error: Utf8Error,
    },
}
