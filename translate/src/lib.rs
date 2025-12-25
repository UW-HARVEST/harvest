//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use harvest_core::config::Config;
use harvest_core::edit::{self, NewEditError};
use harvest_core::tools::MightWriteOutcome;
use harvest_core::{HarvestIR, diagnostics};
use identify_project_kind::IdentifyProjectKind;
use load_raw_source::LoadRawSource;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::{SpawnToolError, ToolRunner};
use scheduler::{NextInvocationOutcome, Scheduler};
use std::sync::Arc;
use tracing::{debug, error, info};
use try_cargo_build::TryCargoBuild;

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<Arc<HarvestIR>, Box<dyn std::error::Error>> {
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir_organizer = edit::Organizer::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();
    scheduler.queue_invocation(LoadRawSource::new(&config.input));
    scheduler.queue_invocation(IdentifyProjectKind);
    scheduler.queue_invocation(TryCargoBuild);
    scheduler.queue_invocation(RawSourceToCargoLlm);
    loop {
        let snapshot = ir_organizer.snapshot();
        scheduler.next_invocations(|mut tool| {
            use NextInvocationOutcome::{DontTryAgain, Error, TryLater};
            let name = tool.name();
            let might_write = match tool.might_write(snapshot.as_ref().into()) {
                MightWriteOutcome::NotRunnable => {
                    debug!("Tool {name} is not runnable");
                    return DontTryAgain;
                }
                MightWriteOutcome::Runnable(might_write) => {
                    debug!("Tool {name} is runnable");
                    might_write
                }
                MightWriteOutcome::TryAgain => {
                    debug!("Tool {name} returned TryAgain");
                    return TryLater(tool);
                }
            };
            match runner.spawn_tool(
                &mut ir_organizer,
                tool,
                snapshot.clone(),
                might_write,
                config.clone(),
            ) {
                Err((SpawnToolError::IoError(error), _)) => {
                    error!("I/O error spawning tool: {error}");
                    Error(SpawnToolError::IoError(error).into())
                }
                Err((SpawnToolError::NewEdit(NewEditError::IdInUse), tool)) => {
                    debug!("Not spawning {name} because an ID it needs is in use.");
                    TryLater(tool)
                }
                Err((SpawnToolError::NewEdit(NewEditError::UnknownId), _)) => {
                    error!("Tool {name}: might_write returned an unknown ID");
                    DontTryAgain
                }
                Ok(()) => {
                    info!("Launched tool {name}");
                    DontTryAgain
                }
            }
        })?;
        if !runner.process_tool_results(&mut ir_organizer) {
            // No tools are running now, which also indicates that no tools are schedulable.
            // Eventually we need some way to determine whether this is a successful outcome or a
            // failure, but for now we can just assume success.
            break;
        }
    }
    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    Ok(ir_organizer.snapshot())
}
