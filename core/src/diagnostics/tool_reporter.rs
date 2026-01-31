//! Diagnostics-reporting infrastructure for tools.

use super::{Shared, SharedWriter, lock_shared};
use crate::tools::Tool;
use std::fmt::{self, Display, Formatter};
use std::fs::create_dir;
use std::io;
use std::num::NonZeroU64;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::{collections::hash_map::Entry, path::PathBuf};
use tracing::dispatcher::{DefaultGuard, set_default};
use tracing::{Dispatch, error, info};
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{Layer as _, Registry};

/// Diagnostics reporter for a specific tool run. These are provided to tools as part of their
/// context.
// TODO: Presumably Tool::might_write also wants a tool-specific reporter. Does it get a general
// Reporter, ToolReporter, or something else? For now I'm not handing a reporter to
// Tool::might_write.
#[derive(Clone)]
pub struct ToolReporter {
    run_shared: Arc<Mutex<RunShared>>,
}

impl ToolReporter {
    /// To construct a ToolReporter, use [Reporter::start_tool_run], which invokes this.
    pub(super) fn new(
        shared: Arc<Mutex<Shared>>,
        tool: &dyn Tool,
    ) -> Result<(ToolJoiner, ToolReporter), io::Error> {
        let (sender, receiver) = channel();
        let tool = ToolId::new(tool);
        let mut guard = lock_shared(&shared);
        let number = match guard.tool_run_counts.entry(tool) {
            Entry::Occupied(mut entry) => {
                let number = entry.get().checked_add(1).unwrap();
                entry.insert(number);
                number
            }
            Entry::Vacant(entry) => *entry.insert(NonZeroU64::MIN),
        };
        let tool_run = ToolRunId {
            tool,
            number,
            _private: (),
        };
        let tool_run_dir = PathBuf::from_iter([
            guard.diagnostics_dir.as_path(),
            "steps".as_ref(),
            tool_run.to_string().as_ref(),
        ]);
        create_dir(&tool_run_dir)?;
        let run_messages_writer = layer()
            .with_ansi(false)
            .with_writer(SharedWriter::new_append(PathBuf::from_iter([
                tool_run_dir.as_path(),
                "messages".as_ref(),
            ]))?);
        let messages_writer = layer()
            .with_ansi(false)
            .with_writer(guard.messages_file.clone());
        let dispatch = Registry::default()
            .with(run_messages_writer)
            .with(messages_writer)
            .with(layer().with_filter(guard.console_filter.clone()))
            .into();
        drop(guard);
        Ok((
            ToolJoiner { receiver },
            ToolReporter {
                run_shared: Arc::new(Mutex::new(RunShared {
                    dispatch,
                    sender,
                    tool_run,
                })),
            },
        ))
    }

    /// Initializes log collection for this thread. Tools should call this for each new thread they
    /// spawn, if they spawn threads. Note that the tool runner sets up the thread logger for the
    /// tool's main thread, so Tools that do not spawn any threads do not need to call this.
    pub fn setup_thread_logger(&self) -> ThreadGuard {
        ThreadGuard {
            _default_guard: set_default(&self.lock_shared().dispatch),
            run_shared: self.run_shared.clone(),
        }
    }

    /// Returns this tool's [ToolId].
    pub fn tool_id(&self) -> ToolId {
        self.tool_run().tool_id()
    }

    /// Returns this invocation's [ToolRunId].
    pub fn tool_run(&self) -> ToolRunId {
        self.lock_shared().tool_run
    }

    /// Utility to lock this reporter's shared reference.
    fn lock_shared(&self) -> MutexGuard<'_, RunShared> {
        match self.run_shared.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                error!("RunShared mutex poisoned");
                self.run_shared.clear_poison();
                poisoned.into_inner()
            }
        }
    }
}

/// Guard returned by [ToolReporter::setup_thread_logger]. Cleans up the thread logger on drop.
pub struct ThreadGuard {
    _default_guard: DefaultGuard,
    /// [ToolJoiner::join] should not return until all ThreadGuards should be dropped, so we hold
    /// onto this reference to keep the [RunShared] alive.
    run_shared: Arc<Mutex<RunShared>>,
}

/// Identifies a particular tool. Conceptually, this is equivalent to the tool's name, but this
/// design allows us to optimize the representation in the future to e.g. use TypeId for faster
/// comparisons and hashing.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ToolId {
    /// The name returned by `Tool::name`.
    name: &'static str,
}

impl ToolId {
    /// Constructs a ToolId for this tool. Note that callers should prefer to construct a ToolId
    /// once and copy it around when possible rather than repeatedly construct `ToolId`s, in case
    /// future optimizations make `new` more expensive to decrease the cost of other operations.
    pub fn new(tool: &dyn Tool) -> ToolId {
        ToolId { name: tool.name() }
    }
}

impl Display for ToolId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        self.name.fmt(f)
    }
}

/// An identifier for a tool run. Can be converted into a string, which will look like
/// `try_cargo_build_2`. This string should be suitable to use as a file/directory name.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ToolRunId {
    pub tool: ToolId,
    /// The first run of a particular tool has number 1, the second has 2, etc.
    pub number: NonZeroU64,

    // Prevents code outside this module from constructing this.
    _private: (),
}

impl Display for ToolRunId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}_{:03}", self.tool, self.number)
    }
}

impl ToolRunId {
    pub fn tool_id(self) -> ToolId {
        self.tool
    }
}

/// A struct that can wait for all diagnostics handles for a tool to be dropped.
pub struct ToolJoiner {
    // Receives a message from RunShared when RunShared is dropped.
    receiver: Receiver<()>,
}

impl ToolJoiner {
    /// Waits until all reporters for this tool run have been dropped. Note that this accepts and
    /// drops the ThreadGuard as well, so that it can emit diagnostics.
    pub fn join(&self, guard: ThreadGuard) {
        if Arc::strong_count(&guard.run_shared) > 1 {
            info!("Waiting for remaining tool reporters to be dropped");
        }
        drop(guard);
        self.receiver
            .recv()
            .expect("sender dropped without sending a message?");
    }
}

/// Data shared between the `ToolReporter`s for a particular tool run.
struct RunShared {
    // tracing dispatcher (this is shared between this tool run's threads).
    dispatch: Dispatch,
    // Used to send a message to ToolJoiner when RunShared is dropped.
    sender: Sender<()>,
    tool_run: ToolRunId,
}

impl Drop for RunShared {
    fn drop(&mut self) {
        let _ = self.sender.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MockTool;

    #[test]
    fn tool_run_id_display() {
        let run_id = ToolRunId {
            tool: ToolId::new(&MockTool::new()),
            number: NonZeroU64::MIN,
            _private: (),
        };
        assert_eq!(run_id.to_string(), "mock_tool_001");
    }
}
