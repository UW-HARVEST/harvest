//! `ModularFixLlm`: an iterative LLM-based repair tool for the modular translation pipeline.
//!
//! It accepts a `CargoPackage` from `ModularTranslationLlm`, tries to build it, and
//! sends any erroneous declarations to an LLM for targeted fixes.  The final (possibly
//! still-imperfect) `CargoPackage` is returned so that `TryCargoBuild` can run a clean
//! final verdict.

mod compiler;
mod error_classifier;
mod fix_llm;
mod history;
mod repair_state;
mod splitter;

use fix_llm::FixLlm;
use full_source::CargoPackage;
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::LLMConfig;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use repair_state::RepairState;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use tempfile::TempDir;
use tracing::{info, warn};

/// Configuration for the modular fix tool, read from `[tools.modular_fix_llm]`.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Maximum number of repair iterations (default: 5).
    #[serde(default = "Config::default_max_iterations")]
    pub max_iterations: usize,

    #[serde(flatten)]
    pub llm: LLMConfig,

    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    fn default_max_iterations() -> usize {
        10
    }

    fn validate(&self) {
        unknown_field_warning("tools.modular_fix_llm", &self.unknown);
    }
}

/// The modular fix tool.
pub struct ModularFixLlm;

impl Tool for ModularFixLlm {
    fn name(&self) -> &'static str {
        "modular_fix_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(context.config.tools.get("modular_fix_llm").ok_or(
            "No modular_fix_llm config found in config.toml. \
                        Please add a [tools.modular_fix_llm] section.",
        )?)?;
        config.validate();

        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("ModularFixLlm: no CargoPackage found in IR")?;

        let mut state = RepairState::from_cargo_package(cargo_package)?;
        let fix_llm = FixLlm::new(&config.llm)?;
        let history_dir = context.config.output.join("repair_history");
        let temp_dir = TempDir::new()?;

        info!(
            "ModularFixLlm: starting repair loop (max {} iterations, {} declarations)",
            config.max_iterations,
            state.declarations.len()
        );

        for iteration in 0..=config.max_iterations {
            let pkg = state.to_cargo_package()?;
            pkg.materialize(temp_dir.path())?;

            let build_output = compiler::run_cargo_build(temp_dir.path())?;
            let classification = error_classifier::classify_errors(&build_output);

            // Persist this iteration's snapshot.
            let source_snapshot = state.assemble_source();
            let errors_opt = if build_output.success {
                None
            } else {
                Some(&classification)
            };
            if let Err(e) =
                history::save_iteration(&history_dir, iteration, &source_snapshot, errors_opt)
            {
                warn!(
                    "ModularFixLlm: failed to write history for iteration {}: {}",
                    iteration, e
                );
            }

            if build_output.success || classification.total_errors == 0 {
                info!("ModularFixLlm: build succeeded at iteration {}", iteration);
                break;
            }

            info!(
                "ModularFixLlm: iteration {}: {} errors in {} files",
                iteration,
                classification.total_errors,
                classification.files.len()
            );

            if iteration == config.max_iterations {
                warn!(
                    "ModularFixLlm: reached max_iterations={}, returning best-effort result",
                    config.max_iterations
                );
                break;
            }

            // Map each error to its declaration by line number.
            let mut decl_errors: HashMap<usize, Vec<String>> = HashMap::new();

            for file_report in &classification.files {
                for diag in &file_report.diagnostics {
                    match state.find_declaration_for_line(diag.line as usize) {
                        Some(idx) => {
                            decl_errors
                                .entry(idx)
                                .or_default()
                                .push(diag.full_text.clone());
                        }
                        None => {
                            warn!(
                                "ModularFixLlm: cannot map error on line {} to a declaration",
                                diag.line
                            );
                        }
                    }
                }
            }

            if decl_errors.is_empty() {
                warn!("ModularFixLlm: errors exist but none could be mapped to declarations");
                break;
            }

            // Compute interface context once per iteration (shared across all fix calls,
            // enabling LLM prefix caching).
            let interface_context = state.interface_context();

            // Send each erroneous declaration to the fix LLM.
            let mut fixed_count = 0usize;
            for (&decl_idx, error_texts) in &decl_errors {
                let decl_source = state.declarations[decl_idx].source.clone();
                let errors_text = error_texts.join("\n\n");

                match fix_llm.fix_declaration(&decl_source, &errors_text, &interface_context) {
                    Ok(fixed) if !fixed.is_empty() => {
                        state.update_declaration(decl_idx, fixed);
                        fixed_count += 1;
                    }
                    Ok(_) => {
                        warn!(
                            "ModularFixLlm: LLM returned empty response for declaration {}",
                            decl_idx
                        );
                    }
                    Err(e) => {
                        warn!(
                            "ModularFixLlm: LLM fix failed for declaration {}: {}",
                            decl_idx, e
                        );
                    }
                }
            }

            info!(
                "ModularFixLlm: iteration {} patched {}/{} declarations",
                iteration,
                fixed_count,
                decl_errors.len()
            );
        }

        Ok(Box::new(state.to_cargo_package()?))
    }
}
