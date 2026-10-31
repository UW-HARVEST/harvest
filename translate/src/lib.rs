//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_c_artifact::BuildCArtifact;
use build_config::BuildConfig;
use build_project_spec::{BuildProjectSpec, ProjectKind, ProjectSpec};
use c_ast::ParseToAst;
use emit_build_features::EmitBuildFeatures;
use fix_declarations_llm::FixDeclarationsLlm;
use fix_diff_failures::FixDiffFailures;
use full_source::CargoPackage;
use generate_difftest_suite::GenerateDiffTestSuite;
use generate_exec_difftests::GenerateExecDifftests;
use harvest_core::config::Config;
use harvest_core::tools::Tool;
use harvest_core::utils::{empty_writable_dir, get_version};
use harvest_core::{HarvestIR, Id, diagnostics};
use load_cargo_package::LoadCargoPackage;
use load_raw_source::LoadRawSource;
use modular_translation_llm::ModularTranslationLlm;
use quantize_rust_spans::QuantizeRustSpans;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use run_difftest::{DiffTestResult, RunDiffTest};
use run_exec_difftest::RunExecDiffTest;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use tracing::info;
use translate_agentic::TranslateAgentic;
use try_cargo_build::{CargoBuildResult, TryCargoBuild};
use verify_fix_agentic::VerifyFixAgentic;
use write_output::WriteOutput;

/// Stage subdirectory names inside the `-o` output workspace. The output path
/// is treated as a workspace anchor: the translation stage writes to
/// `<output>/translated` and the verification stage to `<output>/verified`, so
/// the two stages are self-describing and compose without the caller wiring up
/// any input/output plumbing.
const TRANSLATED_SUBDIR: &str = "translated";
const VERIFIED_SUBDIR: &str = "verified";

/// Returns true if `dir` looks like it already holds a translated crate (has a
/// `Cargo.toml`). Used to decide whether the verify stage can load an existing
/// translation instead of re-running translation from scratch.
fn has_existing_crate(dir: &std::path::Path) -> bool {
    dir.join("Cargo.toml").is_file()
}

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    info!("Harvest version: {}", get_version());
    info!("Transpiling with: {}", config.model_info().unwrap());

    // The `-o` path is a workspace anchor: translation writes to
    // `<output>/translated`, verification writes to `<output>/verified`.
    let translated_dir = config.output.join(TRANSLATED_SUBDIR);
    let verified_dir = config.output.join(VERIFIED_SUBDIR);

    // Resume seam: when the verify stage is requested and a translation already
    // exists at `<output>/translated`, load it instead of re-translating. This
    // lets `--agentic` then `--agentic-verify` compose on the same `-o` without
    // repeating the (expensive) translation. If no prior translation exists,
    // fall back to translating first, then verifying.
    let resume_verify = config.agentic && config.agentic_verify && has_existing_crate(&translated_dir);

    // Prepare the output workspace. We only empty the stage subdirs we are
    // about to (re)write, so a resume-verify run preserves `<output>/translated`
    // (its input) while still clearing a stale `<output>/verified`.
    // `--force` controls whether a nonempty target subdir is erased vs errored.
    std::fs::create_dir_all(&config.output)?;
    if !resume_verify {
        // A fresh translation (with or without verify) rewrites translated/.
        empty_writable_dir(&translated_dir, config.force)?;
    }
    if config.agentic && config.agentic_verify {
        empty_writable_dir(&verified_dir, config.force)?;
    }

    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let build_cfg = scheduler.queue_after(BuildConfig, &[load_src]);
    let project_spec = scheduler.queue_after(BuildProjectSpec, &[load_src, build_cfg]);

    // `translate_stage_pkg` is the CargoPackage id of the translation-only
    // result (whether freshly translated or loaded from disk); `Some` only when
    // that result should be (re)written to `<output>/translated`. When we
    // resume from an existing translation we do not rewrite it.
    let mut translate_stage_pkg: Option<Id> = None;

    let translate = if resume_verify {
        info!(
            "Resuming: loading existing translation from {} (skipping translate stage)",
            translated_dir.display()
        );
        let loaded = scheduler.queue(LoadCargoPackage::new(&translated_dir));
        scheduler.queue_after(VerifyFixAgentic, &[loaded, load_src, build_cfg])
    } else if config.agentic {
        let t = scheduler.queue_after(TranslateAgentic, &[load_src, project_spec, build_cfg]);
        if config.agentic_verify {
            translate_stage_pkg = Some(t);
            scheduler.queue_after(VerifyFixAgentic, &[t, load_src, build_cfg])
        } else {
            t
        }
    } else if config.modular {
        // ParseToAst takes BuildConfigIR as a second input so it can stamp
        // each TopLevelEntity with its variant_tags. When the IR is empty the
        // tags collapse to `Vec::new()` and serialized output is byte-equal
        // to the form produced without a BuildConfigIR input
        // (see TopLevelEntity::variant_tags docs).
        let parse_ast = scheduler.queue_after(ParseToAst, &[load_src, build_cfg]);
        scheduler.queue_after(
            ModularTranslationLlm,
            &[load_src, parse_ast, project_spec, build_cfg],
        )
    } else {
        scheduler.queue_after(RawSourceToCargoLlm, &[load_src, project_spec, build_cfg])
    };
    // EmitBuildFeatures consumes the translated CargoPackage plus the
    // BuildConfigIR and produces a (possibly mutated) CargoPackage. On
    // is_empty IRs (projects without a `configuration.json`, which is
    // the vast majority of the current TRACTOR corpus) it is a no-op
    // pass-through, so byte-for-byte behavior is preserved.
    let translate = scheduler.queue_after(EmitBuildFeatures, &[translate, build_cfg]);
    let mut current_pkg_id = translate;
    let mut current_build_id = scheduler.queue_after(TryCargoBuild, &[current_pkg_id]);

    // Which subdir the FINAL result goes to: verify runs -> verified/, else translated/.
    let final_dir = if config.agentic && config.agentic_verify {
        verified_dir
    } else {
        translated_dir.clone()
    };

    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
        scheduler.run_all(&mut runner, &mut ir, config.clone())?;

        // Repair loop -- skipped for agentic, which has its own repair mechanism.
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

        // Differential testing: generate a test harness that exercises the public API (for
        // library projects) or the program's argv/stdin surface (for executables) through
        // both the original C build and the translated Rust candidate, and repair
        // candidates that fail.
        let is_library = matches!(
            ir.get::<ProjectSpec>(project_spec)
                .ok_or("transpile: no ProjectSpec in IR")?
                .kind,
            ProjectKind::Library
        );
        // The built-in differential test builds the original C with CMake, so it
        // only applies to CMake projects. For autotools/make projects graded by
        // an external harness, `internal_difftest = false` skips it while still
        // writing the translated Rust output below.
        if config.internal_difftest {
            let c_artifact = scheduler.queue_after(BuildCArtifact, &[load_src, project_spec]);
            if is_library {
                let (pkg, build) = run_diff_test_and_repair(
                    &mut scheduler,
                    &mut runner,
                    &mut ir,
                    &config,
                    load_src,
                    c_artifact,
                    current_pkg_id,
                    current_build_id,
                    || GenerateDiffTestSuite,
                    || RunDiffTest,
                )?;
                current_pkg_id = pkg;
                current_build_id = build;
            } else {
                let (pkg, build) = run_diff_test_and_repair(
                    &mut scheduler,
                    &mut runner,
                    &mut ir,
                    &config,
                    load_src,
                    c_artifact,
                    current_pkg_id,
                    current_build_id,
                    || GenerateExecDifftests,
                    || RunExecDiffTest,
                )?;
                current_pkg_id = pkg;
                current_build_id = build;
            }
        }

        // When we freshly translated AND then verified in the same run, also
        // persist the translation-only crate to `<output>/translated` so both
        // stages exist as independent, labeled outputs and a later verify can
        // resume from it. (Skipped when resuming, since translated/ already
        // exists on disk.) Build it first so WriteOutput has a CargoBuildResult.
        if let Some(pre_id) = translate_stage_pkg {
            let pre_build = scheduler.queue_after(TryCargoBuild, &[pre_id]);
            scheduler
                .queue_after(WriteOutput::to(config.output.join(TRANSLATED_SUBDIR)), &[pre_build]);
        }

        // Write the final result to its stage subdir (verified/ or translated/).
        scheduler.queue_after(WriteOutput::to(final_dir.clone()), &[current_build_id]);
        scheduler.run_all(&mut runner, &mut ir, config)?;

        Ok(())
    })();

    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    result?;
    Ok(ir)
}

/// Runs one differential-test generate+run pass, then repeats an LLM-based repair loop
/// (matching the build-repair loop's shape) until the candidate passes or
/// `config.max_diff_repair_passes` is exhausted. Returns the (possibly updated) package and
/// build Ids for the caller to use going forward (e.g. for `WriteOutput`).
///
/// `make_generate`/`make_run` construct fresh instances of the generate-suite and run-test
/// tools for this project kind (library vs executable) -- each `queue_after` call needs its
/// own owned `Tool` value, and the run-test tool is queued multiple times across repair passes.
#[allow(clippy::too_many_arguments)]
fn run_diff_test_and_repair<G: Tool, R: Tool>(
    scheduler: &mut Scheduler,
    runner: &mut ToolRunner,
    ir: &mut HarvestIR,
    config: &Arc<Config>,
    load_src: Id,
    c_artifact: Id,
    mut current_pkg_id: Id,
    mut current_build_id: Id,
    make_generate: impl FnOnce() -> G,
    make_run: impl Fn() -> R,
) -> Result<(Id, Id), Box<dyn std::error::Error>> {
    let diff_suite = scheduler.queue_after(make_generate(), &[load_src]);
    let mut diff_result_id =
        scheduler.queue_after(make_run(), &[diff_suite, c_artifact, current_pkg_id]);
    scheduler.run_all(runner, ir, config.clone())?;
    let mut best_passed = ir
        .get::<DiffTestResult>(diff_result_id)
        .ok_or("transpile: no DiffTestResult in IR")?
        .passed;

    for _ in 0..config.max_diff_repair_passes {
        let failed = ir
            .get::<DiffTestResult>(diff_result_id)
            .ok_or("transpile: no DiffTestResult in IR")?
            .failed;
        if failed == 0 {
            break;
        }

        let fix =
            scheduler.queue_after(FixDiffFailures, &[diff_result_id, load_src, current_pkg_id]);
        scheduler.run_all(runner, ir, config.clone())?;

        // FixDiffFailures can itself hard-error (observed: the LLM returning a file
        // list with a non-relative path, which RawDir::set_file rejects). If it does,
        // `fix` never lands in the IR. Queuing TryCargoBuild/RunDiffTest on `fix`
        // regardless would permanently strand them -- their input can never become
        // ready -- which the scheduler treats as fatal (run_all errors out) rather
        // than recoverable. Check first, and treat a missing `fix` as a rejected
        // attempt, same as a downstream tool failing.
        if ir.get::<CargoPackage>(fix).is_none() {
            continue;
        }

        let new_build = scheduler.queue_after(TryCargoBuild, &[fix]);
        let new_diff_result_id = scheduler.queue_after(make_run(), &[diff_suite, c_artifact, fix]);
        scheduler.run_all(runner, ir, config.clone())?;

        // The run-test tool (unlike TryCargoBuild) hard-errors instead of encoding failure
        // in its result -- e.g. if the LLM's patch doesn't build as a cdylib. The
        // ToolRunner swallows that error and just never inserts the representation, so a
        // missing Id here means "this repair attempt failed," not "the pipeline is broken."
        // Treat it as a rejected candidate and keep iterating from the last-accepted state.
        let Some(new_result) = ir.get::<DiffTestResult>(new_diff_result_id) else {
            continue;
        };
        if new_result.passed > best_passed {
            best_passed = new_result.passed;
            current_pkg_id = fix;
            current_build_id = new_build;
            diff_result_id = new_diff_result_id;
        }
    }

    let diff_result = ir
        .get::<DiffTestResult>(diff_result_id)
        .ok_or("transpile: no DiffTestResult in IR")?;
    info!(
        "Diff test: {}/{} passed ({} failed)",
        diff_result.passed, diff_result.total, diff_result.failed
    );

    Ok((current_pkg_id, current_build_id))
}

#[cfg(not(miri))]
#[cfg(test)]
mod emit_build_features_tests {
    //! Scheduler-level smoke test for [`emit_build_features::EmitBuildFeatures`].
    //!
    //! Lives here (not in `tools/emit_build_features/tests/`) because the
    //! `Scheduler`/`ToolRunner` types are private to this crate. The test
    //! confirms the no-op short-circuit when `BuildConfigIR.is_empty == true`:
    //! the `CargoPackage` is forwarded byte-for-byte. That is the contract
    //! the vast majority of the current TRACTOR corpus depends on.
    use crate::runner::ToolRunner;
    use crate::scheduler::Scheduler;
    use build_config::BuildConfigIR;
    use emit_build_features::EmitBuildFeatures;
    use full_source::CargoPackage;
    use harvest_core::HarvestIR;
    use harvest_core::config::Config;
    use harvest_core::diagnostics::Collector;
    use harvest_core::fs::RawDir;
    use harvest_core::test_util::MockTool;
    use std::sync::Arc;

    /// The canonical `Cargo.toml` body the mock CargoPackage carries through.
    /// On the no-op path EmitBuildFeatures must produce the exact same bytes.
    const CARGO_TOML: &[u8] =
        b"[package]\nname = \"noop_smoke\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";

    fn mock_cargo_package() -> CargoPackage {
        let mut dir = RawDir::default();
        dir.set_file("Cargo.toml", CARGO_TOML.to_vec()).unwrap();
        CargoPackage { dir }
    }

    #[test]
    fn emit_build_features_is_noop_on_empty_ir() -> Result<(), Box<dyn std::error::Error>> {
        let config = Arc::new(Config::mock());
        let collector = Collector::initialize(&config).unwrap();
        let mut runner = ToolRunner::new(collector.reporter());
        let mut ir = HarvestIR::default();

        let mut scheduler = Scheduler::default();
        let pkg_id = scheduler.queue(
            MockTool::new()
                .name("mock_cargo_package")
                .run(|_, _| Ok(Box::new(mock_cargo_package()))),
        );
        let cfg_id =
            scheduler.queue(MockTool::new().name("mock_build_config_empty").run(|_, _| {
                Ok(Box::new(BuildConfigIR {
                    is_empty: true,
                    ..Default::default()
                }))
            }));
        let out_id = scheduler.queue_after(EmitBuildFeatures, &[pkg_id, cfg_id]);

        scheduler.run_all(&mut runner, &mut ir, config.clone())?;

        let out_pkg = ir
            .get::<CargoPackage>(out_id)
            .expect("EmitBuildFeatures must produce a CargoPackage");
        // Byte-for-byte equality with the input: the no-op contract.
        assert_eq!(
            out_pkg.dir.get_file("Cargo.toml").unwrap(),
            CARGO_TOML,
            "no-op path must not mutate Cargo.toml"
        );
        // No build.rs must have been added.
        assert!(
            out_pkg.dir.get_file("build.rs").is_err(),
            "no-op path must not emit a build.rs"
        );
        Ok(())
    }
}
