//! `FixDeclarationsLlm`: calls the LLM to repair declarations that have compiler errors,
//! producing an updated `SplitPackage` with fixed declarations and a recomputed line index.

use full_source::CargoPackage;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use quantize_rust_spans::RustItemMap;
use serde::Deserialize;
use tracing::info;
use try_cargo_build::CargoBuildResult;

mod attribution;
mod fix_llm;
mod interface_ctx;
mod patches;

/// Calls the LLM to fix each declaration that has compiler errors, and returns a new
/// `SplitPackage` with updated declarations and a freshly computed line index.
pub struct FixDeclarationsLlm;

impl Tool for FixDeclarationsLlm {
    fn name(&self) -> &'static str {
        "fix_declarations_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config =
            fix_llm::Config::deserialize(context.config.tools.get("fix_declarations_llm").ok_or(
                "No fix_declarations_llm config found in config.toml. \
                     Please add a [tools.fix_declarations_llm] section.",
            )?)?;
        config.validate();

        let fix_llm = fix_llm::FixLlm::new(&config.llm)?;

        let item_map = context
            .ir_snapshot
            .get::<RustItemMap>(inputs[0])
            .ok_or("DiagnosticAttributor: no RustItemMap found in IR")?;

        let mut cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(item_map.cargo_pkg_idx)
            .ok_or("DiagnosticAttributor: no CargoPackage found in IR")?
            .clone();

        let build_result = context
            .ir_snapshot
            .get::<CargoBuildResult>(inputs[1])
            .ok_or("DiagnosticAttributor: no CargoBuildResult found in IR")?;

        // Group compiler errors by enclosing declaration so each declaration can be fixed once
        let decl_errors = attribution::attribute_errors(build_result, item_map, &cargo_package)?;

        // Build declaration-only context (with stubbed bodies) to guide LLM fixes
        let interface_ctx = interface_ctx::get_interface_ctx(item_map, &cargo_package);

        // Use the LLM to generate patches
        let (mut fixes, fixed_count) =
            patches::generate_patches(&decl_errors, &cargo_package, &fix_llm, &interface_ctx)?;

        // Apply all generated patches into source files
        for (file_name, patch_set) in fixes.drain() {
            let source = cargo_package.dir.get_file_mut(&file_name)?;
            patches::apply_patches(source, patch_set);
        }

        info!(
            "FixDeclarationsLlm: Applied fixes to {}/{} declarations",
            fixed_count,
            decl_errors.len()
        );

        Ok(Box::new(cargo_package))
    }
}
