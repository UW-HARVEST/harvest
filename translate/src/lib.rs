//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use harvest_core::config::Config;
use harvest_core::{HarvestIR, diagnostics};
use identify_project_kind::{IdentifyProjectKind, ProjectKind};
use load_raw_source::LoadRawSource;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use tracing::error;
use try_cargo_build::TryCargoBuild;

struct FixedProjectKind(ProjectKind);

impl harvest_core::tools::Tool for FixedProjectKind {
    fn name(&self) -> &'static str {
        "fixed_project_kind"
    }

    fn run(
        self: Box<Self>,
        _context: harvest_core::tools::RunContext,
        _inputs: Vec<harvest_core::Id>,
    ) -> Result<Box<dyn harvest_core::Representation>, Box<dyn std::error::Error>> {
        Ok(Box::new(self.0))
    }
}

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let identify_kind = if let Some(kind) = config.project_kind {
        let pk = match kind {
            harvest_core::config::ProjectKindOverride::Executable => ProjectKind::Executable,
            harvest_core::config::ProjectKindOverride::Library => ProjectKind::Library,
        };
        scheduler.queue_after(FixedProjectKind(pk), &[load_src])
    } else {
        scheduler.queue_after(IdentifyProjectKind, &[load_src])
    };
    let translate = scheduler.queue_after(RawSourceToCargoLlm, &[load_src, identify_kind]);
    let _try_build = scheduler.queue_after(TryCargoBuild, &[translate]);

    // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
    let result = scheduler.run_all(&mut runner, &mut ir, config);
    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    if let Err(e) = result {
        error!("Error during transpilation: {e}");
        return Err(e);
    }
    Ok(ir)
}
