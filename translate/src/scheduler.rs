//! # harvest_translate scheduler
//!
//! The scheduler is responsible for determining which tools to invoke and also
//! for invoking them.

use harvest_core::tools::Tool;
use std::mem::replace;
use tracing::debug;

#[derive(Default)]
pub struct Scheduler {
    queued_invocations: Vec<Box<dyn Tool>>,
}

impl Scheduler {
    /// Invokes `f` with the next suggested tool invocations. `f` is expected to try to run each
    /// tool. If the tool cannot be executed and should be tried again later, then `f` should
    /// return it.
    pub fn next_invocations<F: FnMut(Box<dyn Tool>) -> NextInvocationOutcome>(
        &mut self,
        mut f: F,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let new_queue = Vec::with_capacity(self.queued_invocations.len());
        for tool in replace(&mut self.queued_invocations, new_queue) {
            use NextInvocationOutcome::{DontTryAgain, Error, TryLater};
            debug!("Trying to invoke tool {}", tool.name());
            match f(tool) {
                DontTryAgain => debug!("Tool removed from queue"),
                TryLater(tool) => {
                    debug!("Returning {} to queue", tool.name());
                    self.queued_invocations.push(tool);
                }
                Error(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Add a tool invocation to the scheduler's queue. Note that scheduling a
    /// tool invocation does not guarantee the tool will run, as a tool may
    /// indicate that it is not runnable.
    pub fn queue_invocation<T: Tool>(&mut self, invocation: T) {
        self.queued_invocations.push(Box::new(invocation));
    }
}

pub enum NextInvocationOutcome {
    /// Indicates the scheduler should not attempt this tool invocation again (this could indicate
    /// either a successful tool run, or a tool invocation that will never succeeed).
    DontTryAgain,
    /// Indicates this tool invocation should be tried again later, after other tool invocations
    /// have completed.
    TryLater(Box<dyn Tool>),
    /// Reports an error that `next_invocations` should immediately return.
    Error(Box<dyn std::error::Error>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use harvest_core::test_util::MockTool;

    #[test]
    fn next_invocation() {
        // Counters for the number of times the scheduler tries to run each tool invocation.
        let [mut a_count, mut b_count] = [0, 0];
        let mut scheduler = Scheduler::default();
        scheduler.queue_invocation(MockTool::new().name("a"));
        scheduler.queue_invocation(MockTool::new().name("b"));
        scheduler
            .next_invocations(|t| match t.name() {
                "a" => {
                    a_count += 1;
                    NextInvocationOutcome::DontTryAgain
                }
                "b" => {
                    b_count += 1;
                    NextInvocationOutcome::TryLater(t)
                }
                _ => panic!("unexpected tool invocation {}", t.name()),
            })
            .expect("incorrect next_invocations error");
        assert_eq!([a_count, b_count], [1, 1]);
        scheduler
            .next_invocations(|t| match t.name() {
                "b" => {
                    b_count += 1;
                    NextInvocationOutcome::DontTryAgain
                }
                _ => panic!("unexpected tool invocation {}", t.name()),
            })
            .expect("incorrect next_invocations error");
        assert_eq!([a_count, b_count], [1, 2]);
        scheduler
            .next_invocations(|t| panic!("unexpected tool invocation {}", t.name()))
            .expect("incorrect next_invocations error");
    }
}
