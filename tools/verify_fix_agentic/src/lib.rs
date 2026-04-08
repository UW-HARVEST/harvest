//! Agentic verify-and-fix tool.
//!
//! After an initial translation, this tool materializes the [`CargoPackage`](full_source::CargoPackage)
//! into a fresh working directory alongside the original C source, then invokes an external agent.
//! The agent compiles and runs both the C and Rust implementations against generated test inputs,
//! compares their outputs, and iteratively fixes the Rust code until the two agree (or the agent
//! gives up). This is dynamic, execution-based verification, not a static or formal analysis.

use full_source::{CargoPackage, RawSource};
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

const PROMPT_VERIFY: &str = include_str!("prompt_verify.md");
const PROMPT_CLAUDE_VERIFY: &str = include_str!("prompt_claude_verify.md");

pub struct VerifyFixAgentic;

impl Tool for VerifyFixAgentic {
    fn name(&self) -> &'static str {
        "verify_fix_agentic"
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
                .get("verify_fix_agentic")
                .unwrap_or(&default_config),
        )?;
        config.validate();

        let agent = context.config.agentic_agent;

        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("No CargoPackage representation found in IR")?;
        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[1])
            .ok_or("No RawSource representation found in IR")?;

        // case_dir/
        //   translated_rust/          <- materialized CargoPackage
        //     c_src/                  <- materialized RawSource (for agent reference)
        let work_dir = tempfile::tempdir()?;
        let case_dir = work_dir.path();
        let translated = case_dir.join("translated_rust");
        cargo_package.dir.materialize(&translated)?;

        let c_src_dir = translated.join("c_src");
        fs::create_dir_all(&c_src_dir)?;
        raw_source.dir.materialize(&c_src_dir)?;

        info!("Working directory: {}", case_dir.display());

        let cmake_flags = extract_cmake_flags(case_dir);
        let prompt =
            load_verify_prompt(&config, agent)?.replace("{CMAKE_BUILD_FLAGS}", &cmake_flags);

        // Kiro runs in case_dir (references translated_rust/ in prompt paths).
        // Claude runs in translated_rust/ directly (references c_src/ and src/).
        let agent_work_dir = match agent {
            AgentKind::Kiro => {
                let p = prompt.replace("{CASE_DIR}", &case_dir.to_string_lossy());
                invoke_agent(case_dir, &p, config.timeout_secs, agent)?;
                case_dir.to_path_buf()
            }
            AgentKind::Claude => {
                write_claude_sandbox(case_dir)?;
                invoke_agent(&translated, &prompt, config.timeout_secs, agent)?;
                translated.clone()
            }
        };
        let _ = agent_work_dir;
        info!("Verification complete");

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

/// Loads the verify prompt for the given agent kind.
fn load_verify_prompt(
    config: &Config,
    agent: AgentKind,
) -> Result<String, Box<dyn std::error::Error>> {
    match agent {
        AgentKind::Claude => match &config.prompt_claude_verify {
            Some(p) => Ok(fs::read_to_string(p)?),
            None => Ok(PROMPT_CLAUDE_VERIFY.to_owned()),
        },
        AgentKind::Kiro => match &config.prompt_verify {
            Some(p) => Ok(fs::read_to_string(p)?),
            None => Ok(PROMPT_VERIFY.to_owned()),
        },
    }
}

/// Invokes the verification agent in `work_dir` with the given prompt and timeout.
fn invoke_agent(
    work_dir: &Path,
    prompt: &str,
    timeout_secs: u64,
    agent: AgentKind,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Invoking verification agent ({agent}, timeout={timeout_secs}s)");

    let logs_dir = work_dir.parent().unwrap_or(work_dir).join("logs");
    fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join("verify.log");
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
        warn!("Verification agent exited with {status}");
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

/// Extracts CMake cache variable flags from `CMakePresets.json`, if present.
///
/// These flags are injected into the verify prompt so the agent knows which build configuration
/// was active for this case.
fn extract_cmake_flags(case_dir: &Path) -> String {
    let presets = case_dir.join("translated_rust/c_src/CMakePresets.json");
    let content = match fs::read_to_string(&presets) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let data: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let Some(cv) = data
        .pointer("/configurePresets/1/cacheVariables")
        .and_then(|v| v.as_object())
    else {
        return String::new();
    };

    cv.iter()
        .filter(|(k, _)| *k != "CMAKE_C_STANDARD" && *k != "CMAKE_BUILD_TYPE")
        .map(|(k, v)| format!("-D{}={}", k, v.as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tool-specific configuration, read from `[tools.verify_fix_agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Override path for the Kiro verification prompt.
    pub prompt_verify: Option<PathBuf>,

    /// Override path for the Claude verification prompt.
    pub prompt_claude_verify: Option<PathBuf>,

    /// Agent timeout in seconds. Defaults to 2700 (45 minutes).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

fn default_timeout_secs() -> u64 {
    2700
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.verify_fix_agentic", &self.unknown);
    }
}
