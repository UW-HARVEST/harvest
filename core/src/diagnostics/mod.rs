//! Provides interfaces for writing and inspecting diagnostics. Diagnostics are collected into two
//! places:
//!
//! 1. The diagnostics directory (if one is configured)
//! 2. The [Diagnostics] struct, which is returned by `transpile`.
//!
//! This module also provides directories for tools to use, as those directories live under the
//! diagnostic directory.

#[cfg(all(not(miri), test))]
mod tests;
mod tool_reporter;

use crate::HarvestIR;
use crate::config::Config;
use crate::fs::DirEntry;
use crate::tools::Tool;
use crate::utils::{EmptyDirError, empty_writable_dir};
use std::collections::HashMap;
use std::fmt::{Arguments, Write as _};
use std::fs::{File, canonicalize, create_dir, write};
use std::io::{self, IoSlice, Write};
use std::mem::replace;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use tempfile::{TempDir, tempdir};
use thiserror::Error;
use tool_reporter::ToolId;
use tracing::{dispatcher::DefaultGuard, error, info, subscriber::set_default};
use tracing_subscriber::filter::ParseError;
use tracing_subscriber::fmt::{MakeWriter, layer};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{EnvFilter, Layer as _, Registry};

pub use tool_reporter::{Scratch, ToolJoiner, ToolReporter};

/// Diagnostics produced by transpilation. Can be used by callers of `transpile` to inspect the
/// diagnostics produced during its execution.
pub struct Diagnostics {
    // TODO: Figure out what we want to have here, versus only on disk. From
    // https://github.com/betterbytes-org/harvest-code/issues/51#issuecomment-3524208160,
    // we at least want information on tool invocation results (successes and errors with error
    // messages).

    // TODO: If this needs to access the diagnostics directory, then we need to move
    // Option<TempDir> from `Collector` into here.
}

impl Diagnostics {
    fn new() -> Diagnostics {
        Diagnostics {}
    }
}

/// Component that collects diagnostics during the execution of `transpile`. Creating a Collector
/// will start collecting `tracing` events (writing them into log files and echoing some events to
/// stdout).
pub struct Collector {
    // When the Shared is destructed, the Diagnostics will be sent to this Collector through this
    // receiver.
    diagnostics_receiver: Receiver<Diagnostics>,
    shared: Arc<Mutex<Shared>>,

    // Guards that clean up values on drop.
    _tempdir: Option<TempDir>,
    _tracing_guard: DefaultGuard,
}

impl Collector {
    /// Creates a Collector, starting diagnostics collection.
    pub fn initialize(config: &Config) -> Result<Collector, CollectorNewError> {
        // We canonicalize the diagnostics path because it will be used to construct paths that are
        // passed as to external commands (as command-line arguments), and the canonicalized path
        // is probably the most compatible representation.
        let (diagnostics_dir, _tempdir) = match &config.diagnostics_dir {
            None => {
                let tempdir = tempdir()?;
                (canonicalize(tempdir.path()), Some(tempdir))
            }
            Some(path) => {
                empty_writable_dir(path, config.force)?;
                (canonicalize(path), None)
            }
        };
        let diagnostics_dir = diagnostics_dir.expect("invalid diagnostics path?");
        create_dir(PathBuf::from_iter([
            diagnostics_dir.as_path(),
            "ir".as_ref(),
        ]))?;
        create_dir(PathBuf::from_iter([
            diagnostics_dir.as_path(),
            "steps".as_ref(),
        ]))?;
        let (diagnostics_sender, diagnostics_receiver) = channel();
        let messages_file = SharedWriter::new_append(PathBuf::from_iter([
            diagnostics_dir.as_path(),
            "messages".as_ref(),
        ]))?;
        let console_filter = EnvFilter::builder().parse(&config.log_filter)?;
        let _tracing_guard = set_default(
            Registry::default()
                .with(layer().with_ansi(false).with_writer(messages_file.clone()))
                .with(layer().with_filter(console_filter.clone())),
        );
        Ok(Collector {
            diagnostics_receiver,
            shared: Arc::new(Mutex::new(Shared {
                console_filter,
                diagnostics: Diagnostics::new(),
                diagnostics_dir,
                diagnostics_sender,
                messages_file,
                tool_run_counts: HashMap::new(),
            })),
            _tempdir,
            _tracing_guard,
        })
    }

    /// Waits until all reporters have been dropped, then consumes this [Collector], extracting the
    /// collected diagnostics.
    pub fn diagnostics(self) -> Diagnostics {
        if Arc::strong_count(&self.shared) > 1 {
            info!("Waiting for remaining reporters to be dropped");
        }
        drop(self.shared);
        self.diagnostics_receiver
            .recv()
            .expect("no Diagnostics sent")
    }

    /// Returns a new [Reporter] that passes diagnostics to this Collector.
    pub fn reporter(&self) -> Reporter {
        Reporter {
            shared: self.shared.clone(),
        }
    }
}

/// A handle used to report diagnostics. Created by using `Collector::reporter`.
#[derive(Clone)]
pub struct Reporter {
    shared: Arc<Mutex<Shared>>,
}

impl Reporter {
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
        let _ = path;
        todo!()
    }

    /// Reports a new version of the IR.
    pub fn report_ir_version(&self, version: u64, snapshot: &HarvestIR) {
        let shared = lock_shared(&self.shared);
        let mut path = shared.diagnostics_dir.clone();
        path.push("ir");
        path.push(format!("{version:03}"));
        if let Err(error) = create_dir(&path) {
            error!("Failed to create IR directory: {error}");
            return;
        }
        let mut types = vec![];
        for (id, repr) in snapshot.iter() {
            let id_string = format!("{:03}", Into::<u64>::into(id));
            path.push(&id_string);
            if let Err(error) = repr.materialize(&path) {
                error!("Failed to materialize repr: {error}");
            }
            path.pop();
            types.push((id, id_string, repr.name()));
        }
        // TODO: For now, HarvestIR does not guarantee a particular iteration order, but it
        // *happens* to iterate in this same order. We should figure out what guarantees we want
        // HarvestIR to have, and then update this accordingly.
        types.sort_unstable_by_key(|t| t.0);
        let mut index = String::new();
        for (_, id_string, name) in types {
            let _ = writeln!(index, "{id_string}: {name}");
        }
        path.push("index");
        if let Err(error) = write(path, index) {
            error!("Failed to write IR index: {error}");
        }
    }

    /// Reports the start of a tool's execution.
    pub fn start_tool_run(&self, tool: &dyn Tool) -> Result<(ToolJoiner, ToolReporter), io::Error> {
        ToolReporter::new(self.shared.clone(), tool)
    }
}

/// Error type returned by Collector::new.
#[derive(Debug, Error)]
pub enum CollectorNewError {
    #[error("diagnostics directory error")]
    DiagnosticsEmptyDir(#[from] EmptyDirError),
    #[error("I/O error")]
    IoError(#[from] io::Error),
    #[error("invalid log_filter")]
    LogFilterError(#[from] ParseError),
}

/// Utility to lock one of the `Shared` references, logging an error if it is poisoned (and
/// unpoisoning it).
fn lock_shared<'m>(shared: &'m Mutex<Shared>) -> MutexGuard<'m, Shared> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            error!("diagnostics mutex poisoned");
            shared.clear_poison();
            poisoned.into_inner()
        }
    }
}

/// Values shared by the Collector and various diagnostics handles. This is contained in an Option,
/// which is set to `None` when [Collector::diagnostics] is called (and must remain Some() until
/// then).
struct Shared {
    console_filter: EnvFilter,
    diagnostics: Diagnostics,
    // Path to the root of the diagnostics directory structure.
    diagnostics_dir: PathBuf,
    // Channel to send the Diagnostics to the Collector when this Shared is dropped.
    diagnostics_sender: Sender<Diagnostics>,

    // Writer for $diagnostic_dir/messages
    messages_file: SharedWriter<File>,

    // The number of times each tool has been run. Tools that have not been run yet will not be
    // present in this map. This is incremented when a tool run starts, not when it ends.
    tool_run_counts: HashMap<ToolId, NonZeroU64>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        let _ = self
            .diagnostics_sender
            .send(replace(&mut self.diagnostics, Diagnostics::new()));
    }
}

/// MakeWriter is not implemented for Arc<Mutex<_>>
/// (https://github.com/tokio-rs/tracing/issues/2687). This works around that by wrapping
/// Arc<Mutex<_>>.
struct SharedWriter<W: Write>(Arc<Mutex<W>>);

impl SharedWriter<File> {
    /// Creates a new file in append mode.
    pub fn new_append<P: AsRef<Path>>(path: P) -> Result<SharedWriter<File>, io::Error> {
        Ok(SharedWriter(Arc::new(
            File::options()
                .append(true)
                .create_new(true)
                .open(path)?
                .into(),
        )))
    }
}

impl<W: Write> Clone for SharedWriter<W> {
    fn clone(&self) -> SharedWriter<W> {
        SharedWriter(self.0.clone())
    }
}

impl<'l, W: Write + 'l> MakeWriter<'l> for SharedWriter<W> {
    type Writer = MutexGuardWriter<'l, W>;
    fn make_writer(&'l self) -> MutexGuardWriter<'l, W> {
        MutexGuardWriter(match self.0.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.0.clear_poison();
                poisoned.into_inner()
            }
        })
    }
}

struct MutexGuardWriter<'l, W: Write>(MutexGuard<'l, W>);

impl<W: Write> Write for MutexGuardWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
    fn write_vectored(&mut self, bufs: &[IoSlice]) -> io::Result<usize> {
        self.0.write_vectored(bufs)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }
    fn write_fmt(&mut self, args: Arguments) -> io::Result<()> {
        self.0.write_fmt(args)
    }
}
