//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_c_artifact::BuildCArtifact;
use build_project_spec::BuildProjectSpec;
use c_ast::ParseToAst;
use fix_declarations_llm::FixDeclarationsLlm;
use fix_diff_failures::FixDiffFailures;
use generate_difftest_suite::GenerateDiffTestSuite;
use harvest_core::config::Config;
use harvest_core::utils::get_version;
use harvest_core::{HarvestIR, diagnostics};
use load_raw_source::LoadRawSource;
use modular_translation_llm::ModularTranslationLlm;
use quantize_rust_spans::QuantizeRustSpans;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use run_difftest::{DiffTestResult, RunDiffTest};
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use tracing::info;
use translate_agentic::TranslateAgentic;
use try_cargo_build::{CargoBuildResult, TryCargoBuild};
use verify_fix_agentic::VerifyFixAgentic;
use write_output::WriteOutput;

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
    let project_spec = scheduler.queue_after(BuildProjectSpec, &[load_src]);

    // Diff test suite generation and C library build run in parallel with translation.
    let diff_test_suite = scheduler.queue_after(GenerateDiffTestSuite, &[load_src]);
    let c_library = scheduler.queue_after(BuildCArtifact, &[load_src, project_spec]);
    let translate = if config.agentic {
        let t = scheduler.queue_after(TranslateAgentic, &[load_src, project_spec]);
        if config.agentic_verify {
            scheduler.queue_after(VerifyFixAgentic, &[t, load_src])
        } else {
            t
        }
    } else if config.modular {
        let parse_ast = scheduler.queue_after(ParseToAst, &[load_src]);
        scheduler.queue_after(ModularTranslationLlm, &[load_src, parse_ast, project_spec])
    } else {
        scheduler.queue_after(RawSourceToCargoLlm, &[load_src, project_spec])
    };
    let mut current_pkg_id = translate;
    let mut current_build_id = scheduler.queue_after(TryCargoBuild, &[current_pkg_id]);

    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
        scheduler.run_all(&mut runner, &mut ir, config.clone())?;

        // Repair loop — skipped for agentic, which has its own repair mechanism.
        if !config.agentic {
            for _ in 0..config.max_repair_passes {
                let success = ir
                    .get::<CargoBuildResult>(current_build_id)
                    .ok_or("transpile: no CargoBuildResult in IR")?
                    .success;
                if success {
                    break;
                }
                let quantize = scheduler.queue_after(QuantizeRustSpans, &[current_pkg_id]);
                let fix = scheduler.queue_after(FixDeclarationsLlm, &[quantize, current_build_id]);
                let new_build = scheduler.queue_after(TryCargoBuild, &[fix]);
                scheduler.run_all(&mut runner, &mut ir, config.clone())?;
                current_pkg_id = fix;
                current_build_id = new_build;
            }
        }

        // Diff-repair loop — skipped for agentic (which has its own verify step) and when
        // diff test prerequisites did not produce output (e.g. non-library project or LLM
        // failure). The loop tracks the best-so-far CargoPackage by pass count; regressions
        // are implicitly discarded by not advancing the best pointers.
        let mut best_build_id = current_build_id;
        if !config.agentic && ir.contains_id(diff_test_suite) && ir.contains_id(c_library) {
            let mut best_cargo_id = current_pkg_id;
            let mut best_diff_result_id =
                scheduler.queue_after(RunDiffTest, &[diff_test_suite, c_library, current_pkg_id]);
            scheduler.run_all(&mut runner, &mut ir, config.clone())?;

            for _ in 0..config.max_diff_repair_passes {
                let failed = ir
                    .get::<DiffTestResult>(best_diff_result_id)
                    .ok_or("transpile: no DiffTestResult in IR")?
                    .failed;
                if failed == 0 {
                    break;
                }
                let fix = scheduler.queue_after(
                    FixDiffFailures,
                    &[best_diff_result_id, load_src, best_cargo_id],
                );
                let new_build = scheduler.queue_after(TryCargoBuild, &[fix]);
                scheduler.run_all(&mut runner, &mut ir, config.clone())?;

                if !ir
                    .get::<CargoBuildResult>(new_build)
                    .is_some_and(|r| r.success)
                {
                    continue;
                }

                let new_result_id =
                    scheduler.queue_after(RunDiffTest, &[diff_test_suite, c_library, fix]);
                scheduler.run_all(&mut runner, &mut ir, config.clone())?;

                let old_passed = ir
                    .get::<DiffTestResult>(best_diff_result_id)
                    .ok_or("transpile: no best DiffTestResult")?
                    .passed;
                let new_passed = ir
                    .get::<DiffTestResult>(new_result_id)
                    .ok_or("transpile: no new DiffTestResult")?
                    .passed;
                if new_passed > old_passed {
                    best_cargo_id = fix;
                    best_build_id = new_build;
                    best_diff_result_id = new_result_id;
                }
            }
        }

        scheduler.queue_after(WriteOutput, &[best_build_id]);
        scheduler.run_all(&mut runner, &mut ir, config)?;

        Ok(())
    })();

    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    result?;
    Ok(ir)
}
