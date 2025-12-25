use harvest_core::diagnostics::Reporter;
use harvest_core::edit::{self, NewEditError};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Edit, HarvestIR, Id};
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::io;
use std::iter::once;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle, ThreadId, spawn};
use thiserror::Error;
use tracing::error;

/// Spawns off each tool execution in its own thread, and keeps track of those threads.
pub struct ToolRunner {
    invocations: HashMap<ThreadId, RunningInvocation>,

    // Diagnostic fields.
    // IR version number. The version start at 0 and increments by 1 every time an IR edit is
    // successfully applied.
    ir_version: u64,
    reporter: Reporter,

    // Channel used by threads to signal that they are completed running.
    receiver: Receiver<ThreadId>,
    sender: Sender<ThreadId>,
}

impl ToolRunner {
    /// Creates a new ToolRunner.
    pub fn new(reporter: Reporter) -> ToolRunner {
        let (sender, receiver) = channel();
        ToolRunner {
            invocations: HashMap::new(),
            ir_version: 0,
            reporter,
            receiver,
            sender,
        }
    }

    /// Waits until at least one tool has completed running, then process the results of all
    /// completed tool invocations. This will update the IR value in edit_organizer. Returns `true`
    /// if at least one tool completed, and `false` if no tools are currently running.
    pub fn process_tool_results(&mut self, edit_organizer: &mut edit::Organizer) -> bool {
        if self.invocations.is_empty() {
            return false;
        }
        for thread_id in
            once(self.receiver.recv().expect("sender dropped")).chain(self.receiver.try_iter())
        {
            let invocation = self
                .invocations
                .remove(&thread_id)
                .expect("missing invocation");
            let completed_invocation = invocation
                .join_handle
                .join()
                .expect("tool invocation thread panicked");
            let Ok(edit) = completed_invocation else {
                continue;
            };
            if let Err(error) = edit_organizer.apply_edit(edit) {
                error!("Edit application error: {error:?}");
                continue;
            }
            self.ir_version += 1;
            self.reporter
                .report_ir_version(self.ir_version, &edit_organizer.snapshot());
        }
        true
    }

    /// Runs a tool. The tool is run in a new thread.
    pub fn spawn_tool(
        &mut self,
        edit_organizer: &mut edit::Organizer,
        tool: Box<dyn Tool>,
        ir_snapshot: Arc<HarvestIR>,
        might_write: HashSet<Id>,
        config: Arc<harvest_core::config::Config>,
    ) -> Result<(), (SpawnToolError, Box<dyn Tool>)> {
        let mut edit = match edit_organizer.new_edit(&might_write) {
            Err(error) => return Err((error.into(), tool)),
            Ok(edit) => edit,
        };
        let sender = self.sender.clone();
        let (tool_joiner, tool_reporter) = match self.reporter.start_tool_run(&*tool) {
            Err(error) => return Err((error.into(), tool)),
            Ok(joiner_reporter) => joiner_reporter,
        };
        let join_handle = spawn(move || {
            let logger = tool_reporter.setup_thread_logger();
            // Tool::run is not necessarily unwind safe, which means that if it panics it might
            // leave shared data in a state that violates invariants. Types that are shared between
            // threads can generally handle this (e.g. Mutex and RwLock have poisoning), but
            // non-Sync types can sometimes have problems there. We don't want to require Tool::run
            // to be unwind safe, so instead this function needs to make sure that values *in this
            // same thread* that `tool` might touch are appropriately dropped/forgotten if `run`
            // panics.
            let result = catch_unwind(AssertUnwindSafe(|| {
                tool.run(RunContext {
                    ir_edit: &mut edit,
                    ir_snapshot,
                    config,
                    reporter: tool_reporter,
                })
                .map(|_| edit)
            }));
            // TODO: Diagnostics module.
            let out = match result {
                Err(panic_error) => {
                    error!("Tool panicked: {panic_error:?}");
                    Err(())
                }
                Ok(Err(tool_error)) => {
                    error!("Tool invocation failed: {tool_error}");
                    Err(())
                }
                Ok(Ok(edit)) => Ok(edit),
            };
            tool_joiner.join(logger);
            let _ = sender.send(thread::current().id());
            out
        });
        self.invocations
            .insert(join_handle.thread().id(), RunningInvocation { join_handle });
        Ok(())
    }
}

/// An error returned from spawn_tool.
#[derive(Debug, Error)]
pub enum SpawnToolError {
    #[error("I/O error")]
    IoError(#[from] io::Error),
    #[error("failed to create Edit")]
    NewEdit(#[from] NewEditError),
}

/// Data the ToolRunner tracks for each currently-running thread. These are accessed from the main
/// thread.
struct RunningInvocation {
    join_handle: JoinHandle<Result<Edit, ()>>,
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use crate::MightWriteOutcome::Runnable;
    use crate::diagnostics::Collector;
    use harvest_core::Representation;
    use harvest_core::config::Config;
    use harvest_core::edit::{self, NewEditError};
    use harvest_core::test_util::MockTool;
    use std::fmt::{self, Display, Formatter};

    struct TestRepresentation;
    impl Display for TestRepresentation {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            write!(f, "TestRepresentation")
        }
    }
    impl Representation for TestRepresentation {
        fn name(&self) -> &'static str {
            "test"
        }
    }

    #[test]
    fn new_edit_errors() {
        let collector = Collector::initialize(&Config::mock()).unwrap();
        let mut edit_organizer = edit::Organizer::default();
        let mut edit = edit_organizer.new_edit(&[].into()).unwrap();
        let config = Arc::new(Config::mock());
        let [a, b, c] = [(); 3].map(|_| edit.add_representation(Box::new(TestRepresentation)));
        edit_organizer.apply_edit(edit).expect("setup edit failed");
        let mut runner = ToolRunner::new(collector.reporter());
        let unknown_id = Id::new();
        let snapshot = edit_organizer.snapshot();
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new()
                .might_write(move |_| Runnable([a, unknown_id].into()))
                .boxed(),
            snapshot.clone(),
            [a, unknown_id].into(),
            config.clone(),
        );
        assert!(matches!(
            result.err().map(|(e, _)| e),
            Some(SpawnToolError::NewEdit(NewEditError::UnknownId))
        ));
        let (sender, receiver) = channel();
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new()
                .might_write(move |_| Runnable([b, c].into()))
                .run(move |_| receiver.recv().map_err(Into::into))
                .boxed(),
            snapshot.clone(),
            [a, b].into(),
            config.clone(),
        );
        assert!(result.is_ok());
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new()
                .might_write(move |_| Runnable([b, c].into()))
                .boxed(),
            snapshot,
            [b, c].into(),
            config.clone(),
        );
        assert!(
            matches!(
                result.err().map(|(e, _)| e),
                Some(SpawnToolError::NewEdit(NewEditError::IdInUse))
            ),
            "spawned tool with in-use ID"
        );
        sender.send(()).expect("receiver dropped");
        runner.process_tool_results(&mut edit_organizer);
    }

    #[test]
    fn replaced_edit() {
        let collector = Collector::initialize(&Config::mock()).unwrap();
        let mut edit_organizer = edit::Organizer::default();
        let mut edit = edit_organizer.new_edit(&[].into()).unwrap();
        let a = edit.add_representation(Box::new(TestRepresentation));
        edit_organizer.apply_edit(edit).expect("setup edit failed");
        let mut runner = ToolRunner::new(collector.reporter());
        let (sender, receiver) = channel();
        let snapshot = edit_organizer.snapshot();
        let config = Arc::new(Config::mock());
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new()
                .might_write(move |_| Runnable([a].into()))
                .run(move |c| {
                    *c.ir_edit = receiver.recv()?;
                    Ok(())
                })
                .boxed(),
            snapshot,
            [a].into(),
            config.clone(),
        );
        assert!(result.is_ok());
        // Verify that `a` was marked as in use
        assert!(edit_organizer.new_edit(&[a].into()).err() == Some(NewEditError::IdInUse));
        let mut edit = edit_organizer.new_edit(&[].into()).unwrap();
        let b = edit.add_representation(Box::new(TestRepresentation));
        sender.send(edit).expect("receiver dropped");
        runner.process_tool_results(&mut edit_organizer);
        let ir_ids: Vec<Id> = edit_organizer.snapshot().iter().map(|(id, _)| id).collect();
        // We don't really need this *exact* behavior, but we do need to verify the runner does
        // something reasonable.
        assert_eq!(ir_ids, [a, b]);
    }

    #[test]
    fn success() {
        let collector = Collector::initialize(&Config::mock()).unwrap();
        let mut edit_organizer = edit::Organizer::default();
        let mut runner = ToolRunner::new(collector.reporter());
        let snapshot = edit_organizer.snapshot();
        let config = Arc::new(Config::mock());
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new()
                .run(|c| {
                    c.ir_edit.add_representation(Box::new(TestRepresentation));
                    Ok(())
                })
                .boxed(),
            snapshot,
            [].into(),
            config.clone(),
        );
        assert!(result.is_ok());
        let ir_count = edit_organizer.snapshot().iter().count();
        assert_eq!(ir_count, 0, "edit applied early");
        runner.process_tool_results(&mut edit_organizer);
        let ir_count = edit_organizer.snapshot().iter().count();
        assert_eq!(ir_count, 1, "edit not applied on success");
    }

    #[test]
    fn tool_error() {
        let collector = Collector::initialize(&Config::mock()).unwrap();
        let mut edit_organizer = edit::Organizer::default();
        let mut runner = ToolRunner::new(collector.reporter());
        let snapshot = edit_organizer.snapshot();
        let config = Arc::new(Config::mock());
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new().run(|_| Err("test error".into())).boxed(),
            snapshot,
            [].into(),
            config.clone(),
        );
        assert!(result.is_ok());
        runner.process_tool_results(&mut edit_organizer);
        let ir_count = edit_organizer.snapshot().iter().count();
        assert_eq!(ir_count, 0, "edit applied when tool errored");
    }

    #[test]
    fn tool_panic() {
        let collector = Collector::initialize(&Config::mock()).unwrap();
        let mut edit_organizer = edit::Organizer::default();
        let mut runner = ToolRunner::new(collector.reporter());
        let snapshot = edit_organizer.snapshot();
        let config = Arc::new(Config::mock());
        let result = runner.spawn_tool(
            &mut edit_organizer,
            MockTool::new().run(|_| panic!("test panic")).boxed(),
            snapshot,
            [].into(),
            config.clone(),
        );
        assert!(result.is_ok());
        runner.process_tool_results(&mut edit_organizer);
        let ir_count = edit_organizer.snapshot().iter().count();
        assert_eq!(ir_count, 0, "edit applied when tool panicked");
    }
}
