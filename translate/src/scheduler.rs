//! # harvest_translate scheduler
//!
//! The scheduler is responsible for determining which tools to invoke and also
//! for invoking them.

use crate::runner::ToolRunner;
use crate::tools::Tool;
use harvest_ir::{HarvestIR, Id};
use log::{error, info};
use std::mem::replace;
use std::sync::Arc;

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
        config: Arc<crate::cli::Config>,
    ) {
        loop {
            // Attempt to spawn all queued tool invocations once.
            // Tools that cannot be executed (e.g., because an ID they need is not ready) are
            // returned to the queue to be tried again later.
            let new_queue = Vec::with_capacity(self.queued_invocations.len());
            for (id, inputs, tool) in replace(&mut self.queued_invocations, new_queue) {
                // Inputs are ready when they are all in the IR.
                // If this is not true, return it to queue and try later.
                let inputs_ready = inputs.iter().all(|&input_id| ir.contains_id(input_id));
                if !inputs_ready {
                    log::debug!(
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
                );
                info!("Launched tool {name}");
            }
            // Wait until at least 1 tool has finished, and update the IR.
            if !runner.process_tool_results(ir) {
                // No tools are running now, which also indicates that no tools are schedulable.
                if !self.queued_invocations.is_empty() {
                    error!(
                        "No tools are running, yet tools are still scheduled to run. 
                    Something has gone terrible wrong."
                    )
                }
                break;
            }
        }
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
    use crate::test_util::MockTool;

    #[test]
    fn next_invocation() {
        // Counters for the number of times the scheduler tries to run each tool invocation.
        let [mut a_count, mut b_count] = [0, 0];
        let mut scheduler = Scheduler::default();
        scheduler.queue_invocation(MockTool::new().name("a"));
        scheduler.queue_invocation(MockTool::new().name("b"));
        scheduler.next_invocations(|t| match t.name() {
            "a" => {
                a_count += 1;
                None
            }
            "b" => {
                b_count += 1;
                Some(t)
            }
            _ => panic!("unexpected tool invocation {}", t.name()),
        });
        assert_eq!([a_count, b_count], [1, 1]);
        scheduler.next_invocations(|t| match t.name() {
            "b" => {
                b_count += 1;
                None
            }
            _ => panic!("unexpected tool invocation {}", t.name()),
        });
        assert_eq!([a_count, b_count], [1, 2]);
        scheduler.next_invocations(|t| panic!("unexpected tool invocation {}", t.name()));
    }
}
