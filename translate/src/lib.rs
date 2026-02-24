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
use modular_translation_llm::ModularTranslationLlm;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};
use try_cargo_build::TryCargoBuild;

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // compile_commands mode: use dedicated compilation unit translation
    if let Some(ref cc_path) = config.compile_commands {
        let collector = diagnostics::Collector::initialize(&config)?;

        let cc_dir = cc_path
            .parent()
            .ok_or("compile_commands path has no parent")?
            .to_path_buf();
        let project_root = config.project_root.as_ref().unwrap_or(&cc_dir);

        // Get LLM config from tools.compilation_unit_to_rust_llm, or fallback to raw_source_to_cargo_llm
        let llm_config = {
            use harvest_core::llm::LLMConfig;
            use serde::Deserialize;

            // Try compilation_unit_to_rust_llm first
            if let Some(tool_cfg) = config.tools.get("compilation_unit_to_rust_llm") {
                LLMConfig::deserialize(tool_cfg)?
            } else if let Some(tool_cfg) = config.tools.get("raw_source_to_cargo_llm") {
                // Fallback to raw_source_to_cargo_llm config
                info!(
                    "No compilation_unit_to_rust_llm config found, using raw_source_to_cargo_llm config"
                );
                LLMConfig::deserialize(tool_cfg)?
            } else {
                return Err(
                    "Missing LLM configuration. Please add [tools.compilation_unit_to_rust_llm] or [tools.raw_source_to_cargo_llm] section to config file".into()
                );
            }
        };

        // Check for custom prompt
        let custom_prompt = config
            .tools
            .get("compilation_unit_to_rust_llm")
            .or_else(|| config.tools.get("raw_source_to_cargo_llm"))
            .and_then(|cfg| cfg.get("prompt"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let translation_config = compilation_unit_to_rust_llm::TranslationConfig {
            llm_config: &llm_config,
            custom_prompt: custom_prompt.as_deref(),
            parallel: config.parallel,
            parallelism: config.parallelism,
        };

        // Process all compilation units
        let _results = compilation_unit_to_rust_llm::process_compile_commands(
            cc_path,
            project_root,
            &config.output,
            &translation_config,
        )?;

        collector.diagnostics();
        return Ok(HarvestIR::default());
    }

    // Basic tool setup for non-compile_commands mode
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let identify_kind = scheduler.queue_after(IdentifyProjectKind, &[load_src]);
    let translate = if config.modular {
        let parse_ast = scheduler.queue_after(ParseToAst, &[load_src]);
        scheduler.queue_after(ModularTranslationLlm, &[load_src, parse_ast, identify_kind])
    } else {
        scheduler.queue_after(RawSourceToCargoLlm, &[load_src, identify_kind])
    };
    let _try_build = scheduler.queue_after(TryCargoBuild, &[translate]);

    // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
    let result = scheduler.run_all(&mut runner, &mut ir, config.clone().into());
    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    if let Err(e) = result {
        error!("Error during transpilation: {e}");
        return Err(e);
    }
    Ok(ir)
}
