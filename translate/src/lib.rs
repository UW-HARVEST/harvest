//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use harvest_core::config::Config;
use harvest_core::{HarvestIR, diagnostics};
use identify_project_kind::IdentifyProjectKind;
use load_raw_source::LoadRawSource;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use tracing::error;
use try_cargo_build::TryCargoBuild;

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let identify_kind = scheduler.queue_after(IdentifyProjectKind, &[load_src]);
    let translate = scheduler.queue_after(RawSourceToCargoLlm, &[load_src, identify_kind]);
    let _try_build = scheduler.queue_after(TryCargoBuild, &[translate]);

    // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
    match scheduler.run_all(&mut runner, &mut ir, config) {
        Ok(()) => {
            // Cleanup
            drop(scheduler);
            drop(runner);
            collector.diagnostics(); // TODO: Return this value (see issue 51)
            Ok(ir)
        }
        Err(e) => {
            error!("Error during transpilation: {}", e);
            // Cleanup
            drop(scheduler);
            drop(runner);
            collector.diagnostics(); // TODO: Return this value (see issue 51)
            Err(e)
        }
    }
}
