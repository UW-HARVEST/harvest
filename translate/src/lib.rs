//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_project_spec::BuildProjectSpec;
use build_project_spec::ProjectSpec;
use c_ast::ParseToAst;
use harvest_core::config::Config;
use harvest_core::utils::get_version;
use harvest_core::{HarvestIR, diagnostics};
use load_raw_source::LoadRawSource;
use modular_translation_llm::ModularTranslationLlm;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use tracing::{error, info};
use try_cargo_build::TryCargoBuild;

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    let mut project_irs = translate_project(config)?;
    if project_irs.len() != 1 {
        return Err(format!(
            "transpile expected exactly 1 target, but build_project_spec produced {}",
            project_irs.len()
        )
        .into());
    }

    Ok(project_irs.remove(0))
}

/// Performs project-aware transpilation: build a project spec once, then transpile each inferred
/// target independently.
pub fn translate_project(
    config: Arc<Config>,
) -> Result<Vec<HarvestIR>, Box<dyn std::error::Error>> {
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    info!("Harvest version: {}", get_version());
    info!("Transpiling with: {}", config.model_info().unwrap());

    // Load raw source once, then build project spec once from the full source tree.
    let mut spec_ir = HarvestIR::default();
    let raw_source_id = scheduler.queue(LoadRawSource::new(&config.input));
    let project_spec_id = scheduler.queue_after(BuildProjectSpec, &[raw_source_id]);

    if let Err(e) = scheduler.run_all(&mut runner, &mut spec_ir, config.clone()) {
        error!("Error while building project spec: {e}");
        return Err(e);
    }

    let project_spec = spec_ir
        .get::<ProjectSpec>(project_spec_id)
        .ok_or("No ProjectSpec representation found in IR")?;

    // Iterate targets in the build analyzer's compile order.
    let targets: Vec<_> = project_spec
        .target_order
        .iter()
        .map(|artifact| {
            project_spec
                .targets
                .get(artifact)
                .map(|target| (artifact, target))
                .ok_or_else(|| {
                    format!(
                        "target_order referenced missing target '{}'",
                        artifact.display()
                    )
                })
        })
        .collect::<Result<_, _>>()?;

    let mut project_irs = Vec::with_capacity(targets.len());
    for (artifact_path, target_spec) in targets.iter() {
        let mut ir = HarvestIR::default();

        let target_raw_source = target_spec.sources.clone();
        let target_raw_source_id = ir.add_representation(Box::new(target_raw_source));

        let translate = if config.modular {
            let parse_ast = scheduler.queue_after(ParseToAst, &[target_raw_source_id]);
            scheduler.queue_after(
                ModularTranslationLlm::new(target_spec.kind),
                &[target_raw_source_id, parse_ast],
            )
        } else {
            scheduler.queue_after(
                RawSourceToCargoLlm::new(target_spec.kind),
                &[target_raw_source_id],
            )
        };
        let _try_build = scheduler.queue_after(TryCargoBuild, &[translate]);

        if let Err(e) = scheduler.run_all(&mut runner, &mut ir, config.clone()) {
            error!(
                "Error during target transpilation for '{}': {e}",
                artifact_path.display()
            );
            return Err(e);
        }

        project_irs.push(ir);
    }

    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    Ok(project_irs)
}
