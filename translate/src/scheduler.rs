//! # harvest_translate scheduler
//!
//! The scheduler is responsible for determining which tools to invoke and also
//! for invoking them.

use crate::runner::ToolRunner;
use harvest_core::config::Config;
use harvest_core::tools::Tool;
use harvest_core::{HarvestIR, Id};
use std::mem::replace;
use std::sync::Arc;
use tracing::{debug, info};

#[derive(Default)]
pub struct Scheduler {
    queued_invocations: Vec<(Id, Vec<Id>, Box<dyn Tool>)>,
}

impl Scheduler {
    /// Runs all queued tool invocations in a loop, spawning tools and processing their results
    /// until no tools are running and no tools are schedulable.
    /// Interesting note: Because a tool run can only declare its inputs
    /// during initialization, the order of declaration of tools using `queue_after` enforces a natural
    /// topological order, preventing cycles.
    pub fn run_all(
        &mut self,
        runner: &mut ToolRunner,
        ir: &mut HarvestIR,
        config: Arc<Config>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            // Attempt to spawn all queued tool invocations once.
            // Tools that cannot be executed (e.g., because an ID they need is not ready) are
            // returned to the queue to be tried again later.
            let new_queue = Vec::with_capacity(self.queued_invocations.len());
            for (id, inputs, tool) in replace(&mut self.queued_invocations, new_queue) {
                debug!("Attempting to run tool {}", tool.name());
                // Inputs are ready when they are all in the IR.
                // If this is not true, return it to queue and try later.
                let inputs_ready = inputs.iter().all(|&input_id| ir.contains_id(input_id));
                if !inputs_ready {
                    debug!(
                        "Deferring tool {} because inputs {:?} are not ready",
                        tool.name(),
                        inputs
                    );
                    self.queued_invocations.push((id, inputs, tool));
                    continue;
                }
                let name = tool.name();
                // We manage dependencies here in the scheduler, so `spawn_tool` is infallible
                runner.spawn_tool(
                    tool,
                    Arc::new(ir.clone()),
                    config.clone(),
                    inputs.to_vec(),
                    id,
                )?;
                info!("Launched tool {name}");
            }
            // Wait until at least 1 tool has finished, and update the IR.
            if !runner.process_tool_results(ir) {
                // No tools are running now, which also indicates that no tools are schedulable.
                if !self.queued_invocations.is_empty() {
                    return Err(
                        "No tools are running, yet tools are still scheduled to run. 
                    Something has gone terrible wrong."
                            .into(),
                    );
                }
                break;
            }
        }
        Ok(())
    }

    /// Add a tool invocation (with no dependencies) to the scheduler's queue.
    pub fn queue<T: Tool>(&mut self, invocation: T) -> Id {
        self.queue_after(invocation, &[])
    }

    /// Add a tool invocation to the scheduler's queue.
    /// Only run this tool after the given inputs are available in the IR.
    pub fn queue_after<T: Tool>(&mut self, invocation: T, inputs: &[Id]) -> Id {
        let id = Id::new(); // Reserve an ID for this tool's result
        self.queued_invocations
            .push((id, inputs.to_vec(), Box::new(invocation)));
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harvest_core::config::Config;
    use harvest_core::diagnostics::Collector;
    use harvest_core::test_util::{MockRepresentation, MockTool};

    #[test]
    fn run_to_completion() -> Result<(), Box<dyn std::error::Error>> {
        let config = Arc::new(Config::mock());
        let collector = Collector::initialize(&config).unwrap();
        let mut runner = ToolRunner::new(collector.reporter());
        let mut ir = HarvestIR::default();

        let mut scheduler = Scheduler::default();
        let a_id = scheduler.queue(MockTool::new().name("a"));
        let _b_id = scheduler.queue_after(MockTool::new().name("b"), &[a_id]);
        scheduler.run_all(&mut runner, &mut ir, config.clone())?;

        // Ensure both tools ran to completion
        assert!(scheduler.queued_invocations.is_empty());
        let representations = ir
            .get_by_representation::<MockRepresentation>()
            .map(|(_, r)| r)
            .collect::<Vec<_>>();
        assert!(representations.len() == 2);
        Ok(())
    }
}
