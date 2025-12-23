use super::DiagnosticsDir;
use std::fs::{read, read_to_string};
use std::path::{Path, PathBuf};
use std::str::{Utf8Error, from_utf8};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tracing::error;

// Note: File and TextFile are internally Arc<> to a single shared type. That way, the UTF-8-ness
// of the file can be shared between the copies, because it is computed lazily.
/// A read-only file.
#[derive(Clone, Debug)]
pub struct File {
    shared: Arc<Shared>,
}

impl File {
    pub fn bytes(&self) -> Arc<[u8]> {
        match self.shared.contents() {
            Contents::Utf8(contents) => contents.into(),
            Contents::NotUtf8 { contents, .. } => contents,
        }
    }

    pub fn is_utf8(&self) -> bool {
        <TextFile as TryFrom<_>>::try_from(self.clone()).is_ok()
    }

    pub fn path(&self) -> &Path {
        &self.shared.path
    }
}

impl From<TextFile> for File {
    fn from(file: TextFile) -> File {
        File {
            shared: file.shared.clone(),
        }
    }
}

/// A read-only UTF-8 file.
#[derive(Clone, Debug)]
pub struct TextFile {
    // Invariant: shared.contents is a Contents::Utf8().
    shared: Arc<Shared>,
}

impl TextFile {
    pub fn bytes(&self) -> Arc<[u8]> {
        self.str().into()
    }

    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    pub fn str(&self) -> Arc<str> {
        match self.shared.contents() {
            Contents::Utf8(contents) => contents,
            _ => panic!("non-UTF-8 TextFile"),
        }
    }
}

impl TryFrom<File> for TextFile {
    type Error = Utf8Error;
    fn try_from(file: File) -> Result<TextFile, Utf8Error> {
        let guard = file.shared.lock_contents();
        match *guard {
            CachedContents::Unknown => {}
            CachedContents::Utf8(_) => {
                return Ok(TextFile {
                    shared: file.shared.clone(),
                });
            }
            CachedContents::NotUtf8 { error, .. } => return Err(error),
        }
        match file.shared.load(guard) {
            Contents::Utf8(_) => Ok(TextFile {
                shared: file.shared.clone(),
            }),
            Contents::NotUtf8 { error, .. } => Err(error),
        }
    }
}

/// Data for [File]s and [TextFile]s.
#[derive(Debug)]
struct Shared {
    contents: Mutex<CachedContents>,
    #[allow(dead_code)] // TODO: Remove
    diagnostics_dir: Arc<DiagnosticsDir>,
    path: PathBuf,
}

impl Shared {
    fn contents(&self) -> Contents {
        let mut guard = self.lock_contents();
        match *guard {
            CachedContents::Unknown => {}
            CachedContents::Utf8(ref contents) => match contents.upgrade() {
                None => {
                    let contents = read_to_string(&self.path)
                        .expect("read of frozen text file failed")
                        .into();
                    *guard = CachedContents::Utf8(Arc::downgrade(&contents));
                    return Contents::Utf8(contents);
                }
                Some(contents) => return Contents::Utf8(contents),
            },
            CachedContents::NotUtf8 {
                ref contents,
                error,
            } => match contents.upgrade() {
                None => {
                    let contents = read(&self.path).expect("read of frozen file failed").into();
                    *guard = CachedContents::NotUtf8 {
                        contents: Arc::downgrade(&contents),
                        error,
                    };
                    return Contents::NotUtf8 { contents, error };
                }
                Some(contents) => return Contents::NotUtf8 { contents, error },
            },
        }
        self.load(guard)
    }

    /// Reads in the file contents, updating the cache and returning them.
    fn load(&self, mut guard: MutexGuard<CachedContents>) -> Contents {
        let contents = read(&self.path).expect("read of frozen file failed");
        match from_utf8(&contents) {
            Ok(contents) => {
                let contents = contents.into();
                *guard = CachedContents::Utf8(Arc::downgrade(&contents));
                Contents::Utf8(contents)
            }
            Err(error) => {
                let contents = contents.into();
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

/// This file's contents.
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

/// Cached data about this file's contents.
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
