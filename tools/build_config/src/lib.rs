//! HARVEST tool: interprets CMake build scripts and a
//! `configuration.json` to produces a [`BuildConfigIR`] from the raw
//! C source.
//!
//! Inputs: one [`RawSource`] id.
//! Output: one [`BuildConfigIR`].
//!
//! The IR is the single source of truth for `configuration.json`-driven build
//! variability.

// So far, we just plumb this tool through the scheduler and writes a
// JSON dump to the diagnostics tree; later changes will wire it into
// the translation, verification, and benchmarking paths.

use std::collections::HashMap;

use full_source::RawSource;
use harvest_core::config::unknown_field_warning;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

pub mod build_rs;
pub mod ir;
pub mod prompt_ext;
pub mod scanner;

pub use build_rs::render_build_rs;
pub use ir::{
    BuildConfigIR, ConditionalTarget, ConfigVarKind, ConfigVariable, DefineKind, DefineMapping,
    SourceSelection, SourceVariant,
};
pub use prompt_ext::build_system_prompt;
pub use scanner::scan;

/// This tool has no knobs of its own; the struct exists to absorb
/// unknown keys and warn rather than reject.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.build_config", &self.unknown);
    }
}

pub struct BuildConfig;

impl Tool for BuildConfig {
    fn name(&self) -> &'static str {
        "build_config"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        if let Some(raw) = context.config.tools.get("build_config") {
            let config = Config::deserialize(raw)?;
            config.validate();
        }

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;

        let ir = scanner::scan(&raw_source.dir);
        debug!("build_config: {ir}");
        Ok(Box::new(ir))
    }
}
