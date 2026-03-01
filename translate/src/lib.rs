//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use c_ast::ParseToAst;
use harvest_core::config::Config;
use harvest_core::{HarvestIR, diagnostics};
use identify_project_kind::IdentifyProjectKind;
use load_raw_source::LoadRawSource;
use modular_fix_llm::ModularFixLlm;
use modular_translation_llm::ModularTranslationLlm;
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
    let translate = if config.modular {
        let parse_ast = scheduler.queue_after(ParseToAst, &[load_src]);
        let translated =
            scheduler.queue_after(ModularTranslationLlm, &[load_src, parse_ast, identify_kind]);
        if config.fix {
            scheduler.queue_after(ModularFixLlm, &[translated])
        } else {
            translated
        }
    } else {
        scheduler.queue_after(RawSourceToCargoLlm, &[load_src, identify_kind])
    };
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
