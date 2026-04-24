//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_project_spec::{BuildProjectSpec, ProjectSpec};
use c_ast::ParseToAst;
use harvest_core::config::Config;
use harvest_core::utils::get_version;
use harvest_core::{HarvestIR, diagnostics};
use infer_analysis::InferStaticAnalyze;
use load_raw_source::LoadRawSource;
use modular_translation_llm::ModularTranslationLlm;
use normalize_cargo::NormalizeCargo;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use tracing::{error, info};
use translate_agentic::TranslateAgentic;
use try_cargo_build::TryCargoBuild;
use verify_fix_agentic::VerifyFixAgentic;

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    info!("Harvest version: {}", get_version());
    info!("Transpiling with: {}", config.model_info().unwrap());

    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let infer = scheduler.queue_after(InferStaticAnalyze, &[load_src]);
    /*let project_spec = scheduler.queue_after(BuildProjectSpec, &[load_src]);
    scheduler.run_all(&mut runner, &mut ir, config.clone())?;
    let spec = ir
        .get::<ProjectSpec>(project_spec)
        .ok_or("No ProjectSpec representation found in IR")?;
    let translate = match spec.kind {
        build_project_spec::ProjectKind::Library | build_project_spec::ProjectKind::Executable => {
            let parse_ast = scheduler.queue_after(ParseToAst, &[load_src]);
            let t =
                scheduler.queue_after(ModularTranslationLlm, &[load_src, parse_ast, project_spec]);
            let t = scheduler.queue_after(NormalizeCargo, &[t]);
            scheduler.queue_after(TryCargoBuild, &[t]);
            t
        }
        build_project_spec::ProjectKind::Configurable => {
            let mut t = scheduler.queue_after(TranslateAgentic, &[load_src, project_spec]);
            if config.agentic_verify {
                t = scheduler.queue_after(VerifyFixAgentic, &[t, load_src]);
            }
            t
        }
    };*/

    // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
    let result = scheduler.run_all(&mut runner, &mut ir, config.clone());
    ir.get_representation(infer)
        .ok_or("No CargoPackage representation found in IR")?
        .materialize(&config.output.join("results"))?;
    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    if let Err(e) = result {
        error!("Error during transpilation: {e}");
        return Err(e);
    }
    Ok(ir)
}
