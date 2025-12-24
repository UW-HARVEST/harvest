use super::DiagnosticsDir;
use std::fs::{Permissions, read, read_to_string, set_permissions};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
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
    /// Freezes the given file and returns a new File object referring to it. Note that this is for
    /// internal use by the diagnostics system; other code should use `Reporter::freeze` to create
    /// a File.
    pub(super) fn new(diagnostics_dir: Arc<DiagnosticsDir>, path: PathBuf) -> io::Result<File> {
        let shared = Shared {
            contents: Mutex::new(CachedContents::Unknown),
            diagnostics_dir,
            path,
        };
        // User readable, no other permissions
        set_permissions(shared.path(), Permissions::from_mode(0o400))?;
        Ok(File {
            shared: Arc::new(shared),
        })
    }

    pub fn bytes(&self) -> Arc<[u8]> {
        match self.shared.contents() {
            Contents::Utf8(contents) => contents.into(),
            Contents::NotUtf8 { contents, .. } => contents,
        }
    }

    pub fn is_utf8(&self) -> bool {
        <TextFile as TryFrom<_>>::try_from(self.clone()).is_ok()
    }

    pub fn path(&self) -> PathBuf {
        self.shared.path()
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

    pub fn path(&self) -> PathBuf {
        self.shared.path()
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
    diagnostics_dir: Arc<DiagnosticsDir>,
    // Path to a copy of this file in the filesystem. This path is relative to the diagnostic
    // directory, and does not traverse any symlinks (i.e. if it is appended to
    // diagnostics_dir.path, it creates a canonical path).
    path: PathBuf,
}

impl Shared {
    fn contents(&self) -> Contents {
        let mut guard = self.lock_contents();
        match *guard {
            CachedContents::Unknown => self.load(guard),
            CachedContents::Utf8(ref contents) => match contents.upgrade() {
                None => {
                    let contents = read_to_string(self.path())
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
                    let contents = read(self.path())
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
        println!("{:?}", self.path());
        let contents = read(self.path()).expect("read of frozen file failed");
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

    pub fn path(&self) -> PathBuf {
        PathBuf::from_iter([&self.diagnostics_dir.path, &self.path])
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
