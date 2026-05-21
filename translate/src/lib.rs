//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_c_library::BuildCLibrary;
use build_project_spec::BuildProjectSpec;
use c_ast::ParseToAst;
use fix_declarations_llm::FixDeclarationsLlm;
use generate_difftest_suite::GenerateDiffTestSuite;
use generate_test_suite::GenerateTestSuite;
use run_difftest::RunDiffTest;
use harvest_core::config::Config;
use harvest_core::utils::get_version;
use harvest_core::{HarvestIR, diagnostics};
use load_raw_source::LoadRawSource;
use modular_translation_llm::ModularTranslationLlm;
use quantize_rust_spans::QuantizeRustSpans;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
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

    // Test suite generation and C library build run in parallel with translation.
    let test_suite = scheduler.queue_after(GenerateTestSuite, &[load_src]);
    let diff_test_suite = scheduler.queue_after(GenerateDiffTestSuite, &[test_suite, load_src]);
    let c_library = scheduler.queue_after(BuildCLibrary, &[load_src, project_spec]);
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

        // Baseline diff test run — establishes pass/fail before any diff repair loop.
        let _diff_test_result =
            scheduler.queue_after(RunDiffTest, &[diff_test_suite, c_library, current_pkg_id]);
        scheduler.queue_after(WriteOutput, &[current_build_id]);
        scheduler.run_all(&mut runner, &mut ir, config)?;

        Ok(())
    })();

    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    result?;
    Ok(ir)
}
