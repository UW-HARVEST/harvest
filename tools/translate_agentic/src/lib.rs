//! Agentic translation tool for HARVEST.
//!
//! Translates a C project (as a [`RawSource`](full_source::RawSource)) into a Rust Cargo project
//! by invoking an external agent. After the agent finishes, the generated `Cargo.toml` is
//! post-processed to satisfy downstream tool expectations.
//!
//! The translated project is stored in the IR as a [`CargoPackage`](full_source::CargoPackage),
//! which acts as an immutable snapshot that the optional [`verify_fix_agentic`] tool can consume.

mod cargo_toml;

use build_project_spec::{ProjectKind, ProjectSpec};
use cargo_toml::{self as ct, CargoToml};
use full_source::{CargoPackage, RawSource};
use harvest_core::config::unknown_field_warning;
use harvest_core::fs::RawDir;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, read_dir};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

const PROMPT_EXECUTABLE: &str = include_str!("prompt_executable.md");
const PROMPT_LIBRARY: &str = include_str!("prompt_library.md");

/// Default translation agent timeout in seconds (30 minutes).
const TRANSLATE_TIMEOUT_SECS: u64 = 1800;

pub struct TranslateAgentic;

impl Tool for TranslateAgentic {
    fn name(&self) -> &'static str {
        "translate_agentic"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let default_config = serde_json::Value::Object(Default::default());
        let config = Config::deserialize(
            context
                .config
                .tools
                .get("translate_agentic")
                .unwrap_or(&default_config),
        )?;
        config.validate();

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;
        let project_spec = context
            .ir_snapshot
            .get::<ProjectSpec>(inputs[1])
            .ok_or("No ProjectSpec representation found in IR")?;

        let translate_prompt = load_prompt(
            &config.prompt_executable,
            &config.prompt_library,
            &project_spec.kind,
        )?;

        // Set up a working directory that mirrors the layout the agent expects:
        //   case_dir/translated_rust/c_src/  <- materialized C source
        let work_dir = tempfile::tempdir()?;
        let case_dir = work_dir.path();
        let c_src_dir = case_dir.join("translated_rust/c_src");
        fs::create_dir_all(&c_src_dir)?;
        raw_source.dir.materialize(&c_src_dir)?;

        info!("Working directory: {}", case_dir.display());

        let translated = case_dir.join("translated_rust");
        invoke_agent(&translated, &translate_prompt, TRANSLATE_TIMEOUT_SECS)?;

        if !translated.join("Cargo.toml").exists() {
            return Err("Agent did not produce a Cargo.toml".into());
        }

        post_process(&translated, &project_spec.kind)?;
        info!("Translation complete");

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

/// Invokes the translation agent in `work_dir` with the given prompt and timeout.
fn invoke_agent(
    work_dir: &Path,
    prompt: &str,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Invoking translation agent (timeout={}s)", timeout_secs);

    let logs_dir = work_dir.parent().unwrap_or(work_dir).join("logs");
    fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join("translation.log");

    let status = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -o pipefail; timeout {timeout_secs} kiro-cli chat \
             --no-interactive --trust-all-tools \"$PROMPT\" < /dev/null 2>&1 | tee \"$LOG\"",
        ))
        .env("PROMPT", prompt)
        .env("LOG", &log_path)
        .env(
            "OPENSSL_DIR",
            std::env::var("OPENSSL_DIR").unwrap_or_else(|_| "/usr".into()),
        )
        .current_dir(work_dir)
        .status()?;

    if !status.success() {
        warn!("Translation agent exited with {status}");
    }
    Ok(())
}

/// Applies standard Cargo.toml fixups after the agent finishes.
fn post_process(
    translated: &Path,
    project_kind: &ProjectKind,
) -> Result<(), Box<dyn std::error::Error>> {
    let cargo_path = translated.join("Cargo.toml");
    let mut cargo = CargoToml::open(&cargo_path)?;

    cargo.add_workspace();

    match project_kind {
        ProjectKind::Library => {
            cargo.remove_bin();
            // Use the package name already chosen by the agent as the library name.
            if let Some(name) = read_package_name(&cargo_path) {
                cargo.set_lib(&name);
            }
            cargo.save()?;
            ct::strip_for_lib(translated)?;
        }
        ProjectKind::Executable => {
            cargo.set_bin_driver();
            cargo.save()?;
        }
    }
    Ok(())
}

/// Reads `[package].name` from a Cargo.toml. Returns `None` on any failure.
fn read_package_name(manifest: &Path) -> Option<String> {
    let contents = fs::read_to_string(manifest).ok()?;
    let doc: toml_edit::DocumentMut = contents.parse().ok()?;
    doc.get("package")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
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

/// Tool-specific configuration, read from `[tools.translate_agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Override path for the executable translation prompt.
    pub prompt_executable: Option<PathBuf>,

    /// Override path for the library translation prompt.
    pub prompt_library: Option<PathBuf>,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.translate_agentic", &self.unknown);
    }
}
