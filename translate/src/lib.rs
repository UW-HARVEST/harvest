//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use build_config::BuildConfig;
use build_project_spec::BuildProjectSpec;
use c_ast::ParseToAst;
use emit_build_features::EmitBuildFeatures;
use fix_declarations_llm::FixDeclarationsLlm;
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
    let build_cfg = scheduler.queue_after(BuildConfig, &[load_src]);
    let project_spec = scheduler.queue_after(BuildProjectSpec, &[load_src]);
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
    // EmitBuildFeatures consumes the translated CargoPackage plus the
    // BuildConfigIR and produces a (possibly mutated) CargoPackage. On
    // is_empty IRs (the entire current TRACTOR corpus) it is a no-op pass-
    // through, so byte-for-byte behavior is preserved for projects without
    // a `configuration.json`.
    let translate = scheduler.queue_after(EmitBuildFeatures, &[translate, build_cfg]);
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

#[cfg(not(miri))]
#[cfg(test)]
mod emit_build_features_tests {
    //! Scheduler-level smoke test for [`emit_build_features::EmitBuildFeatures`].
    //!
    //! Lives here (not in `tools/emit_build_features/tests/`) because the
    //! `Scheduler`/`ToolRunner` types are private to this crate. The test
    //! confirms the no-op short-circuit when `BuildConfigIR.is_empty == true`:
    //! the `CargoPackage` is forwarded byte-for-byte. That is the contract the
    //! entire current TRACTOR corpus depends on.
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
