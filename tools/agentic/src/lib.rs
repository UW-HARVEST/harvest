//! Agentic translation tool for HARVEST.
//!
//! This tool wraps the kiro-cli agent workflow (translate + optional verify) as a single HARVEST
//! tool. It accepts a [`RawSource`](full_source::RawSource) and produces a
//! [`CargoPackage`](full_source::CargoPackage).
//!
//! # Phase 1 scope
//!
//! Each invocation handles exactly one case. Multi-config (shared-source) deduplication is
//! intentionally deferred to Phase 2, where HARVEST's own repository splitting will drive the
//! grouping instead of the tool doing it internally.

mod cargo_toml;
mod translate_single;
mod verify_single;

use build_project_spec::{ProjectKind, ProjectSpec};
use full_source::{CargoPackage, RawSource};
use harvest_core::config::unknown_field_warning;
use harvest_core::fs::RawDir;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, read_dir};
use std::path::PathBuf;
use tracing::info;

const PROMPT_EXECUTABLE: &str = include_str!("prompt_executable.md");
const PROMPT_LIBRARY: &str = include_str!("prompt_library.md");
const PROMPT_VERIFY: &str = include_str!("prompt_verify.md");

pub struct AgenticTool;

impl Tool for AgenticTool {
    fn name(&self) -> &'static str {
        "agentic"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(context.config.tools.get("agentic").unwrap())?;
        config.validate();

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;
        let project_spec = context
            .ir_snapshot
            .get::<ProjectSpec>(inputs[1])
            .ok_or("No ProjectSpec representation found in IR")?;

        // Select prompts, preferring user-provided paths over built-in defaults.
        let translate_prompt = load_prompt(&config.prompt_executable, &config.prompt_library, &project_spec.kind)?;
        let verify_prompt = config
            .prompt_verify
            .as_ref()
            .map(fs::read_to_string)
            .transpose()?
            .unwrap_or_else(|| PROMPT_VERIFY.to_owned());

        // Set up a working directory that mirrors the layout the agentic pipeline expects:
        //   case_dir/translated_rust/c_src/  <- materialized C source
        let work_dir = tempfile::tempdir()?;
        let case_dir = work_dir.path();
        let c_src = case_dir.join("translated_rust/c_src");
        fs::create_dir_all(&c_src)?;
        raw_source.dir.materialize(&c_src)?;

        info!("Working directory: {}", case_dir.display());

        // --- translate ---
        if config.do_translate {
            translate_single::translate(case_dir, &translate_prompt, &project_spec.kind)?;
        }

        // --- verify ---
        if config.do_verify {
            verify_single::verify(case_dir, &verify_prompt)?;
        }

        // Read the translated Rust project back into a CargoPackage.
        let translated = case_dir.join("translated_rust");
        if !translated.join("Cargo.toml").exists() {
            return Err("No Cargo.toml in translated output".into());
        }

        // Remove artifacts that should not be carried into the IR.
        let c_src_out = translated.join("c_src");
        if c_src_out.exists() {
            fs::remove_dir_all(&c_src_out)?;
        }
        let target_out = translated.join("target");
        if target_out.exists() {
            fs::remove_dir_all(&target_out)?;
        }

        let (dir, directories, files) = RawDir::populate_from(read_dir(&translated)?)?;
        info!("Produced CargoPackage with {directories} directories and {files} files");

        Ok(Box::new(CargoPackage { dir }))
    }
}

/// Loads the translate prompt for the given project kind.
fn load_prompt(
    prompt_executable: &Option<PathBuf>,
    prompt_library: &Option<PathBuf>,
    kind: &ProjectKind,
) -> Result<String, Box<dyn std::error::Error>> {
    let (config_path, builtin) = match kind {
        ProjectKind::Executable => (prompt_executable, PROMPT_EXECUTABLE),
        ProjectKind::Library => (prompt_library, PROMPT_LIBRARY),
    };
    match config_path {
        Some(p) => Ok(fs::read_to_string(p)?),
        None => Ok(builtin.to_owned()),
    }
}

/// Tool-specific configuration, read from `[tools.agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Whether to run the translation stage. Defaults to `true`.
    #[serde(default = "default_true")]
    pub do_translate: bool,

    /// Whether to run the verification stage after translation. Defaults to `false`.
    #[serde(default)]
    pub do_verify: bool,

    /// Override path for the executable translation prompt.
    pub prompt_executable: Option<PathBuf>,

    /// Override path for the library translation prompt.
    pub prompt_library: Option<PathBuf>,

    /// Override path for the verification prompt.
    pub prompt_verify: Option<PathBuf>,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.agentic", &self.unknown);
    }
}

fn default_true() -> bool {
    true
}
