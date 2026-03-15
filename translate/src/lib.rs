//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_project_spec::BuildProjectSpec;
use c_ast::ParseToAst;
use diagnostic_attributor::DiagnosticAttributor;
use fix_build_check::{FixBuildCheck, RawBuildResult};
use fix_declarations_llm::FixDeclarationsLlm;
use harvest_core::config::Config;
use harvest_core::utils::get_version;
use harvest_core::{HarvestIR, Id, diagnostics};
use load_raw_source::LoadRawSource;
use modular_translation_llm::ModularTranslationLlm;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::ToolRunner;
use scheduler::Scheduler;
use split_and_format::{SplitAndFormat, SplitPackage};
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info, warn};
use try_cargo_build::TryCargoBuild;

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    info!("Harvest version: {}", get_version());
    info!("Transpiling with: {}", config.model_info().unwrap());

    let result = run_pipeline(&mut runner, &mut scheduler, &mut ir, config);

    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)

    if let Err(e) = result {
        error!("Error during transpilation: {e}");
        return Err(e);
    }
    Ok(ir)
}

/// Core pipeline logic. Separated so that `transpile` can handle drops and diagnostics
/// after the pipeline finishes regardless of success or failure.
fn run_pipeline(
    runner: &mut ToolRunner,
    scheduler: &mut Scheduler,
    ir: &mut HarvestIR,
    config: Arc<Config>,
) -> Result<(), Box<dyn std::error::Error>> {
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let project_spec = scheduler.queue_after(BuildProjectSpec, &[load_src]);

    if config.modular {
        let parse_ast = scheduler.queue_after(ParseToAst, &[load_src]);
        let translated =
            scheduler.queue_after(ModularTranslationLlm, &[load_src, parse_ast, project_spec]);

        if config.fix {
            // Phase 1: Initial translation and splitting
            let mut split_id = scheduler.queue_after(SplitAndFormat, &[translated]);
            scheduler.run_all(runner, ir, config.clone())?;

            // Phase 2: Fix loop
            for iteration in 0..config.max_fix_iterations {
                let check_id = scheduler.queue_after(FixBuildCheck, &[split_id]);
                // Must issue the run_all here to get the build result before we can decide whether to continue with the fix loop or not
                scheduler.run_all(runner, ir, config.clone())?;

                // Persist this iteration's source snapshot and errors before potentially breaking or continuing
                save_repair_history(&config.output, iteration, ir, split_id, check_id);

                if ir
                    .get::<RawBuildResult>(check_id)
                    .is_some_and(|r| r.success)
                {
                    info!("Fix loop: build succeeded at iteration {}", iteration);
                    break;
                }

                let diag_id = scheduler.queue_after(DiagnosticAttributor, &[split_id, check_id]);
                let new_split_id = scheduler.queue_after(FixDeclarationsLlm, &[split_id, diag_id]);
                split_id = new_split_id;
                // No need to run the scheduler yet
            }

            // Phase 3: Final verification and output
            let _final = scheduler.queue_after(TryCargoBuild::from_split_package(), &[split_id]);
            scheduler.run_all(runner, ir, config)?;
        } else {
            // No fix loop
            let _try_build = scheduler.queue_after(TryCargoBuild::new(), &[translated]);
            scheduler.run_all(runner, ir, config)?;
        }
    } else {
        // Non-modular
        let translated = scheduler.queue_after(RawSourceToCargoLlm, &[load_src, project_spec]);
        let _try_build = scheduler.queue_after(TryCargoBuild::new(), &[translated]);
        scheduler.run_all(runner, ir, config)?;
    }

    Ok(())
}

/// Write `repair_history/iter_{iteration}/source.rs` (always) and
/// `repair_history/iter_{iteration}/errors.json` (only when the build failed)
/// to the output directory.
fn save_repair_history(
    output_dir: &Path,
    iteration: usize,
    ir: &HarvestIR,
    split_id: Id,
    check_id: Id,
) {
    use std::fs;

    let iter_dir = output_dir
        .join("repair_history")
        .join(format!("iter_{iteration}"));

    if let Err(e) = fs::create_dir_all(&iter_dir) {
        warn!(
            "repair history: failed to create {}: {e}",
            iter_dir.display()
        );
        return;
    }

    if let Some(pkg) = ir.get::<SplitPackage>(split_id)
        && let Err(e) = fs::write(iter_dir.join("source.rs"), &pkg.assembled_source)
    {
        warn!("repair history: failed to write source.rs for iteration {iteration}: {e}");
    }

    if let Some(result) = ir.get::<RawBuildResult>(check_id)
        && !result.success
    {
        match serde_json::to_string_pretty(&result.diagnostics) {
            Ok(json) => {
                if let Err(e) = fs::write(iter_dir.join("errors.json"), json) {
                    warn!(
                        "repair history: failed to write errors.json for iteration {iteration}: {e}"
                    );
                }
            }
            Err(e) => {
                warn!(
                    "repair history: failed to serialize diagnostics for iteration {iteration}: {e}"
                );
            }
        }
    }
}
