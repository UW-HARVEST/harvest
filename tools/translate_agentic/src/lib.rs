//! Agentic translation tool for HARVEST.
//!
//! Translates a C project (as a [`RawSource`](full_source::RawSource)) into a Rust Cargo project
//! by invoking an external agent. After the agent finishes, the generated `Cargo.toml` is
//! post-processed to satisfy downstream tool expectations.
//! The translated project is stored in the IR as a [`CargoPackage`](full_source::CargoPackage).

use build_config::BuildConfigIR;
use build_config::prompt_ext::build_system_prompt;
use build_project_spec::{ProjectKind, ProjectSpec};
use full_source::{CargoPackage, RawSource};
use harvest_core::cargo_utils::{CargoToml, strip_for_lib};
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

/// Agent-only structural notes appended after the variable-listing section
/// when `BuildConfigIR.is_empty == false`. Says only the things that are
/// specific to the agentic translation path -- everything mechanical about
/// the variables themselves comes from `build_system_prompt`.
const AGENTIC_CONSTRAINTS: &str = "\n\
**Notes for the agentic path**:\n\
- Do NOT write a `build.rs` -- `EmitBuildFeatures` generates it after you finish.\n\
- Source files that are selected per-variant (e.g. `backend_${BACKEND}.c`) \
  should each be translated into a separate Rust module annotated with the \
  matching `#[cfg(...)]` rule above.\n";

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
        let build_cfg = context
            .ir_snapshot
            .get::<BuildConfigIR>(inputs[2])
            .ok_or("No BuildConfigIR representation found in IR")?;

        let translate_prompt = load_prompt(
            &config.prompt_executable,
            &config.prompt_library,
            &project_spec.kind,
            build_cfg,
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
        invoke_agent(&translated, &translate_prompt, config.timeout_secs)?;

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
    let mut cargo = CargoToml::open(&translated.join("Cargo.toml"))?;
    cargo.add_workspace();
    match project_kind {
        ProjectKind::Library => {
            cargo.remove_bin();
            // Read the package name from the in-memory doc before overwriting [lib].
            if let Some(name) = cargo.package_name() {
                cargo.set_lib(&name);
            }
            cargo.save()?;
            strip_for_lib(translated)?;
        }
        ProjectKind::Executable => {
            cargo.set_bin_driver();
            cargo.save()?;
        }
    }
    Ok(())
}

/// Loads the translate prompt for the given project kind.
///
/// When a config-path override is provided it is used verbatim. Otherwise the
/// built-in legacy prompt (`prompt_executable.md` / `prompt_library.md`) is
/// the base, and when `BuildConfigIR.is_empty == false` two extensions are
/// appended:
///
/// 1. The shared configurable-variables section materialized from the IR by
///    [`build_system_prompt`]. The variable inventory, default values, and
///    cfg/env attribution rules are listed concretely so the agent does not
///    have to discover them from `c_src/configuration.json`.
/// 2. A small set of agent-specific structural notes
///    ([`AGENTIC_CONSTRAINTS`]) about not writing `build.rs` and emitting
///    per-variant modules.
///
/// When `is_empty == true` the legacy prompt is returned byte-for-byte.
fn load_prompt(
    prompt_executable: &Option<PathBuf>,
    prompt_library: &Option<PathBuf>,
    kind: &ProjectKind,
    build_cfg: &BuildConfigIR,
) -> Result<String, Box<dyn std::error::Error>> {
    let (config_path, legacy) = match kind {
        ProjectKind::Executable => (prompt_executable, PROMPT_EXECUTABLE),
        ProjectKind::Library => (prompt_library, PROMPT_LIBRARY),
    };
    if let Some(p) = config_path {
        // Config-path overrides are used verbatim -- the caller supplies the
        // full prompt and is responsible for any configurable-variables wording.
        return Ok(fs::read_to_string(p)?);
    }
    if build_cfg.is_empty {
        return Ok(legacy.to_owned());
    }
    let with_vars = build_system_prompt(legacy, build_cfg);
    Ok(format!("{with_vars}{AGENTIC_CONSTRAINTS}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use build_config::{BuildConfigIR, ConfigVarKind, ConfigVariable};

    fn empty_ir() -> BuildConfigIR {
        BuildConfigIR {
            is_empty: true,
            ..Default::default()
        }
    }

    fn non_empty_ir() -> BuildConfigIR {
        BuildConfigIR {
            is_empty: false,
            variables: vec![ConfigVariable {
                name: "BACKEND".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["alpha".into(), "beta".into()],
                    numeric: false,
                },
                default: Some("alpha".into()),
            }],
            ..Default::default()
        }
    }

    /// With an empty IR, the executable path returns the legacy prompt verbatim.
    #[test]
    fn load_prompt_empty_ir_executable_returns_legacy() {
        let prompt = load_prompt(&None, &None, &ProjectKind::Executable, &empty_ir()).unwrap();
        assert_eq!(
            prompt, PROMPT_EXECUTABLE,
            "empty IR must return legacy executable prompt byte-for-byte"
        );
    }

    /// With an empty IR, the library path returns the legacy prompt verbatim.
    #[test]
    fn load_prompt_empty_ir_library_returns_legacy() {
        let prompt = load_prompt(&None, &None, &ProjectKind::Library, &empty_ir()).unwrap();
        assert_eq!(
            prompt, PROMPT_LIBRARY,
            "empty IR must return legacy library prompt byte-for-byte"
        );
    }

    /// With a non-empty IR, the executable prompt extends the legacy template
    /// with the materialized variable listing plus the agentic constraints
    /// note. The extension must include the canonical bare-cfg form for the
    /// BACKEND enum (not `feature = "..."`) and must reference the variable
    /// names with the same casing as `BuildConfigIR.variables[].name`.
    #[test]
    fn load_prompt_non_empty_ir_executable_extends_legacy() {
        let prompt = load_prompt(&None, &None, &ProjectKind::Executable, &non_empty_ir()).unwrap();
        assert!(
            prompt.starts_with(PROMPT_EXECUTABLE),
            "extended prompt must start with the legacy executable prompt"
        );
        assert!(prompt.contains("## Configurable variables"));
        assert!(prompt.contains("#[cfg(BACKEND_alpha)]"));
        assert!(prompt.contains("#[cfg(BACKEND_beta)]"));
        assert!(!prompt.contains("feature = \"BACKEND_"));
        assert!(prompt.contains(AGENTIC_CONSTRAINTS));
    }

    /// Same contract for the library path: legacy prefix, variable section,
    /// agentic constraints suffix.
    #[test]
    fn load_prompt_non_empty_ir_library_extends_legacy() {
        let prompt = load_prompt(&None, &None, &ProjectKind::Library, &non_empty_ir()).unwrap();
        assert!(
            prompt.starts_with(PROMPT_LIBRARY),
            "extended prompt must start with the legacy library prompt"
        );
        assert!(prompt.contains("## Configurable variables"));
        assert!(prompt.contains("#[cfg(BACKEND_alpha)]"));
        assert!(prompt.contains(AGENTIC_CONSTRAINTS));
    }

    /// Casing fidelity: a Boolean variable with an uppercase name in the IR
    /// must produce `#[cfg(feature = "ENABLE_EXTRA")]` in the prompt -- the
    /// `EmitBuildFeatures` emitter writes the feature with the configuration
    /// variable's original case, so the cfg gate must match.
    #[test]
    fn load_prompt_non_empty_ir_preserves_boolean_var_casing() {
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![ConfigVariable {
                name: "ENABLE_EXTRA".into(),
                kind: ConfigVarKind::Boolean,
                default: Some("false".into()),
            }],
            ..Default::default()
        };
        let prompt = load_prompt(&None, &None, &ProjectKind::Executable, &ir).unwrap();
        assert!(prompt.contains("#[cfg(feature = \"ENABLE_EXTRA\")]"));
        assert!(!prompt.contains("#[cfg(feature = \"enable_extra\")]"));
    }

    /// A config-path override is used verbatim regardless of IR state.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn load_prompt_config_override_is_verbatim() {
        // Write a tiny sentinel prompt to a tempfile.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("custom.md");
        fs::write(&p, b"CUSTOM_PROMPT").unwrap();

        let prompt = load_prompt(
            &Some(p.clone()),
            &None,
            &ProjectKind::Executable,
            &non_empty_ir(),
        )
        .unwrap();
        assert_eq!(prompt, "CUSTOM_PROMPT");

        // Same override with empty IR -- still verbatim.
        let prompt2 = load_prompt(&Some(p), &None, &ProjectKind::Executable, &empty_ir()).unwrap();
        assert_eq!(prompt2, "CUSTOM_PROMPT");
    }
}

/// Tool-specific configuration, read from `[tools.translate_agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Override path for the executable translation prompt.
    pub prompt_executable: Option<PathBuf>,

    /// Override path for the library translation prompt.
    pub prompt_library: Option<PathBuf>,

    /// Agent timeout in seconds. Defaults to 1800 (30 minutes).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

fn default_timeout_secs() -> u64 {
    1800
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.translate_agentic", &self.unknown);
    }
}
