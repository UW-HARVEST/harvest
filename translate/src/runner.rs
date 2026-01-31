use harvest_core::config::Config;
use harvest_core::diagnostics::Reporter;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{HarvestIR, Id, Representation};
use std::collections::HashMap;
use std::iter::once;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle, ThreadId, spawn};
use tracing::{error, info, trace};

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
    /// completed tool invocations. This will update the IR. Returns `true`
    /// if at least one tool completed, and `false` if no tools are currently running.
    pub fn process_tool_results(&mut self, ir: &mut HarvestIR) -> bool {
        trace!(
            "Processing tool results. Current invocations: {:?}",
            self.invocations.keys()
        );
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
            let Ok(representation) = completed_invocation else {
                continue;
            };
            self.ir_version += 1;
            // Need to add new representation before reporting IR version, so that reporter can see it.
            ir.insert_representation(invocation.id, representation);
            self.reporter.report_ir_version(self.ir_version, ir);
        }
        trace!(
            "Finished processing tool results. Current invocations: {:?}, IR keys: {:?}",
            self.invocations.keys(),
            ir.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        true
    }

    /// Runs a tool in a new thread.
    pub fn spawn_tool(
        &mut self,
        tool: Box<dyn Tool>,
        ir_snapshot: Arc<HarvestIR>,
        config: Arc<Config>,
        tool_inputs: Vec<Id>,
        id: Id,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let sender = self.sender.clone();
        let (tool_joiner, tool_reporter) = self.reporter.start_tool_run(&*tool)?;
        let join_handle = spawn(move || {
            let logger = tool_reporter.setup_thread_logger();
            let tool_run = tool_reporter.tool_run();
            // Tool::run is not necessarily unwind safe, which means that if it panics it might
            // leave shared data in a state that violates invariants. Types that are shared between
            // threads can generally handle this (e.g. Mutex and RwLock have poisoning), but
            // non-Sync types can sometimes have problems there. We don't want to require Tool::run
            // to be unwind safe, so instead this function needs to make sure that values *in this
            // same thread* that `tool` might touch are appropriately dropped/forgotten if `run`
            // panics.
            let result = catch_unwind(AssertUnwindSafe(|| {
                tool.run(
                    RunContext::new(ir_snapshot, config, tool_reporter),
                    tool_inputs,
                )
            }));
            let result = match result {
                Err(panic_error) => {
                    error!("Tool run {tool_run} panicked: {panic_error:?}");
                    Err(())
                }
                Ok(Err(tool_error)) => {
                    error!("Tool run {tool_run} failed: {tool_error}");
                    Err(())
                }
                Ok(Ok(result)) => {
                    info!("Tool run {tool_run} succeeded");
                    Ok(result)
                }
            };
            tool_joiner.join(logger);
            let _ = sender.send(thread::current().id());
            result
        });
        self.invocations.insert(
            join_handle.thread().id(),
            RunningInvocation { id, join_handle },
        );
        trace!(
            "Adding invocation for tool. Current invocations: {:?}",
            self.invocations.keys()
        );
        Ok(())
    }
}

/// Data the ToolRunner tracks for each currently-running thread. These are accessed from the main
/// thread.
struct RunningInvocation {
    id: Id,
    join_handle: JoinHandle<Result<Box<dyn Representation>, ()>>,
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use harvest_core::config::Config;
    use harvest_core::diagnostics::Collector;
    use harvest_core::test_util::MockTool;

    #[test]
    fn success() -> Result<(), Box<dyn std::error::Error>> {
        let config = Arc::new(Config::mock());
        let collector = Collector::initialize(&config).unwrap();
        let mut ir = HarvestIR::default();
        let mut runner = ToolRunner::new(collector.reporter());
        let tool = MockTool::new().boxed();
        let id = Id::new();
        runner.spawn_tool(tool, Arc::new(ir.clone()), config.clone(), Vec::new(), id)?;
        let ir_count = ir.iter().count();
        assert_eq!(ir_count, 0, "ir updated early");
        runner.process_tool_results(&mut ir);
        let ir_count = ir.iter().count();
        assert_eq!(ir_count, 1, "ir not updated on success");
        Ok(())
    }

    #[test]
    fn tool_error() -> Result<(), Box<dyn std::error::Error>> {
        let config = Arc::new(Config::mock());
        let collector = Collector::initialize(&config).unwrap();
        let mut ir = HarvestIR::default();
        let mut runner = ToolRunner::new(collector.reporter());
        let tool = MockTool::new().run(|_, _| Err("test error".into())).boxed();
        let id = Id::new();
        runner.spawn_tool(tool, Arc::new(ir.clone()), config.clone(), Vec::new(), id)?;
        runner.process_tool_results(&mut ir);
        let ir_count = ir.iter().count();
        assert_eq!(ir_count, 0, "ir updated when tool errored");
        Ok(())
    }

    #[test]
    fn tool_panic() -> Result<(), Box<dyn std::error::Error>> {
        let config = Arc::new(Config::mock());
        let collector = Collector::initialize(&config).unwrap();
        let mut ir = HarvestIR::default();
        let mut runner = ToolRunner::new(collector.reporter());
        let tool = MockTool::new().run(|_, _| panic!("test panic")).boxed();
        let id = Id::new();
        runner.spawn_tool(tool, Arc::new(ir.clone()), config.clone(), Vec::new(), id)?;
        runner.process_tool_results(&mut ir);
        let ir_count = ir.iter().count();
        assert_eq!(ir_count, 0, "ir updated when tool panicked");
        Ok(())
    }
}
