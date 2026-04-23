//! Agentic translation tool for HARVEST.
//!
//! Translates a C project (as a [`RawSource`](full_source::RawSource)) into a Rust Cargo project
//! by invoking an external agent. After the agent finishes, the generated `Cargo.toml` is
//! post-processed to satisfy downstream tool expectations.
//! The translated project is stored in the IR as a [`CargoPackage`](full_source::CargoPackage).

use build_project_spec::{ProjectKind, ProjectSpec};
use full_source::{CargoPackage, RawSource};
use harvest_core::cargo_utils::{CargoToml, strip_for_lib};
use harvest_core::config::{AgentKind, unknown_field_warning};
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
const PROMPT_CONFIGURABLE: &str = include_str!("prompt_configurable.md");
const PROMPT_CLAUDE_TRANSLATE: &str = include_str!("prompt_claude_translate.md");

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

        let agent = context.config.agentic_agent;
        let translate_prompt = load_prompt(&config, &project_spec.kind, agent)?;

        // Set up a working directory that mirrors the layout the agent expects:
        //   case_dir/translated_rust/c_src/  <- materialized C source
        let work_dir = tempfile::tempdir()?;
        let case_dir = work_dir.path();
        let c_src_dir = case_dir.join("translated_rust/c_src");
        fs::create_dir_all(&c_src_dir)?;
        raw_source.dir.materialize(&c_src_dir)?;

        info!("Working directory: {}", case_dir.display());

        let translated = case_dir.join("translated_rust");

        // Materialize agent tools if enabled and build the prompt section.
        // When disabled, {AGENT_TOOLS_SECTION} is replaced with an empty string so
        // the entire "Available Tools" block is absent from the prompt the agent sees.
        let agent_tools_section = if context.config.agent_tools {
            let dir = translated.join("agent_tools");
            agent_tools_embed::materialize_to(&dir)?;
            let docs = agent_tools_embed::collect_docs()
                .replace("{AGENT_TOOLS_DIR}", &dir.to_string_lossy());
            format!(
                "## Available Tools\n\n\
                 The following tools are pre-installed in `{}/`. Use them when you\n\
                 need a precise answer about C behavior rather than reasoning from first principles.\n\n\
                 {}\n",
                dir.display(),
                docs
            )
        } else {
            String::new()
        };

        // The wishlist file lives inside the agent's working directory so the agent
        // can write to it without any special permissions. We inject the absolute
        // path into the prompt so the agent knows exactly where to append entries.
        let local_wishlist = translated.join("tool_wishlist.json");
        let translate_prompt = translate_prompt
            .replace("{WISHLIST_PATH}", &local_wishlist.to_string_lossy())
            .replace("{AGENT_TOOLS_SECTION}", &agent_tools_section);

        if agent == AgentKind::Claude {
            write_claude_sandbox(case_dir)?;
        }
        invoke_agent(&translated, &translate_prompt, config.timeout_secs, agent)?;

        // Copy the wishlist out before the tempdir is dropped.
        if local_wishlist.exists() {
            if let Some(out_path) = &config.wishlist_output_path {
                if let Err(e) = fs::copy(&local_wishlist, out_path) {
                    warn!("Failed to copy tool wishlist to {}: {}", out_path.display(), e);
                } else {
                    info!("Tool wishlist written to {}", out_path.display());
                }
            }
        }

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
    agent: AgentKind,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Invoking translation agent ({agent}, timeout={timeout_secs}s)");

    let logs_dir = work_dir.parent().unwrap_or(work_dir).join("logs");
    fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join("translation.log");
    let openssl_dir = std::env::var("OPENSSL_DIR").unwrap_or_else(|_| "/usr".into());

    let status = match agent {
        AgentKind::Kiro => Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -o pipefail; timeout {timeout_secs} kiro-cli chat \
                 --no-interactive --trust-all-tools \"$PROMPT\" < /dev/null 2>&1 | tee \"$LOG\"",
            ))
            .env("PROMPT", prompt)
            .env("LOG", &log_path)
            .env("OPENSSL_DIR", &openssl_dir)
            .current_dir(work_dir)
            .status()?,
        AgentKind::Claude => Command::new("bash")
            .arg("-c")
            .arg(format!(
                "set -o pipefail; timeout {timeout_secs} claude -p \"$PROMPT\" \
                 --allowedTools 'Bash(*)' 'Write' 'Edit' \
                 --max-turns 50 \
                 --output-format stream-json --verbose \
                 < /dev/null 2>&1 | tee \"$LOG\"",
            ))
            .env("PROMPT", prompt)
            .env("LOG", &log_path)
            .env("OPENSSL_DIR", &openssl_dir)
            .current_dir(work_dir)
            .status()?,
    };

    if !status.success() {
        warn!("Translation agent exited with {status}");
    }
    Ok(())
}

/// Writes `.claude/settings.json` to sandbox Claude within the working directory.
fn write_claude_sandbox(case_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let claude_dir = case_dir.join(".claude");
    fs::create_dir_all(&claude_dir)?;
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({
            "sandbox": {
                "enabled": true,
                "allowUnsandboxedCommands": false,
                "filesystem": {
                    "allowRead": [case_dir.to_string_lossy()],
                    "allowWrite": [case_dir.to_string_lossy()]
                }
            }
        })
        .to_string(),
    )?;
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
        ProjectKind::Configurable => {
            // The configurable prompt instructs the agent to produce both [lib] and [[bin]].
            // Only ensure workspace is present (already added above); leave the rest intact.
            cargo.save()?;
        }
    }
    Ok(())
}

/// Loads the translate prompt, selecting between Kiro (per-kind) and Claude (unified) variants.
fn load_prompt(
    config: &Config,
    kind: &ProjectKind,
    agent: AgentKind,
) -> Result<String, Box<dyn std::error::Error>> {
    match agent {
        AgentKind::Claude => match &config.prompt_claude_translate {
            Some(p) => Ok(fs::read_to_string(p)?),
            None => Ok(PROMPT_CLAUDE_TRANSLATE.to_owned()),
        },
        AgentKind::Kiro => {
            let (config_path, builtin) = match kind {
                ProjectKind::Executable => (&config.prompt_executable, PROMPT_EXECUTABLE),
                ProjectKind::Library => (&config.prompt_library, PROMPT_LIBRARY),
                ProjectKind::Configurable => (&config.prompt_configurable, PROMPT_CONFIGURABLE),
            };
            match config_path {
                Some(p) => Ok(fs::read_to_string(p)?),
                None => Ok(builtin.to_owned()),
            }
        }
    }
}

/// Tool-specific configuration, read from `[tools.translate_agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Override path for the Kiro executable translation prompt.
    pub prompt_executable: Option<PathBuf>,

    /// Override path for the Kiro library translation prompt.
    pub prompt_library: Option<PathBuf>,

    /// Override path for the configurable translation prompt.
    pub prompt_configurable: Option<PathBuf>,

    /// Override path for the Claude unified translation prompt.
    pub prompt_claude_translate: Option<PathBuf>,

    /// Agent timeout in seconds. Defaults to 1800 (30 minutes).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,


    /// Destination path for the agent's tool wishlist file.
    /// Injected by the benchmark at runtime (set to <output_dir>/tool_wishlist.json).
    /// If absent, any wishlist the agent writes is silently discarded with the tempdir.
    pub wishlist_output_path: Option<PathBuf>,

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
