//! Agentic verify-and-fix tool.
//!
//! After an initial translation, this tool materializes the [`CargoPackage`](full_source::CargoPackage)
//! into a fresh working directory alongside the original C source, then invokes an external agent.
//! The agent compiles and runs both the C and Rust implementations against generated test inputs,
//! compares their outputs, and iteratively fixes the Rust code until the two agree (or the agent
//! gives up). This is dynamic, execution-based verification, not a static or formal analysis.

use full_source::{CargoPackage, RawSource};
use harvest_core::cmake_presets::{TestConfig, find_test_config};
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
const PROMPT_CLAUDE_VERIFY_NO_PLAN: &str = include_str!("prompt_claude_verify_no_plan.md");

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

        // Materialize agent tools if enabled and build the prompt section.
        // When disabled, {AGENT_TOOLS_SECTION} is replaced with an empty string so
        // the entire "Available Tools" block is absent from the prompt the agent sees.
        let agent_tools_section = if context.config.agent_tools {
            let dir = translated.join("agent_tools");
            agent_tools_embed::materialize_to(&dir)?;
            let docs = agent_tools_embed::collect_docs()
                .replace("{AGENT_TOOLS_DIR}", &dir.to_string_lossy());
            format!(
                "---\n\n## Available Tools\n\n\
                 The following tools are pre-installed in `{}/`. Use them when you\n\
                 need a precise answer about C behavior rather than reasoning from first principles.\n\n\
                 {}\n\n",
                dir.display(),
                docs
            )
        } else {
            String::new()
        };

        // The wishlist file lives inside translated_rust/ so the agent can write to it
        // without any special permissions. The absolute path is injected into the prompt.
        let local_wishlist = translated.join("tool_wishlist.json");

        // Look near the original input dir; the case_dir tempdir does not contain
        // CMakePresets.json because raw_source only mirrors `test_case/` content.
        let test_config = find_test_config(&context.config.input);
        let prompt = load_verify_prompt(&config, agent)?
            .replace("{CMAKE_BUILD_FLAGS}", &test_config.cmake_flags)
            .replace("{ALL_CONFIGURATIONS}", &render_configurations(&test_config))
            .replace("{WISHLIST_PATH}", &local_wishlist.to_string_lossy())
            .replace("{AGENT_TOOLS_SECTION}", &agent_tools_section);

        // Kiro runs in case_dir (references translated_rust/ in prompt paths).
        // Claude runs in translated_rust/ directly (references c_src/ and src/).
        let agent_work_dir = match agent {
            AgentKind::Kiro => {
                let p = prompt.replace("{CASE_DIR}", &case_dir.to_string_lossy());
                invoke_agent(case_dir, &p, config.timeout_secs, agent, config.model.as_deref(), config.no_plan)?;
                case_dir.to_path_buf()
            }
            AgentKind::Claude => {
                write_claude_sandbox(case_dir)?;
                invoke_agent(&translated, &prompt, config.timeout_secs, agent, config.model.as_deref(), config.no_plan)?;
                translated.clone()
            }
        };
        let _ = agent_work_dir;
        info!("Verification complete");

        // Append verify-phase wishlist entries to the translate-phase file (if any),
        // so the final output contains wishes from both phases in chronological order.
        if local_wishlist.exists() {
            if let Some(out_path) = &config.wishlist_output_path {
                match (fs::read_to_string(&local_wishlist), fs::read_to_string(out_path)) {
                    (Ok(new_entries), Ok(existing)) => {
                        let merged = format!("{}{}", existing, new_entries);
                        if let Err(e) = fs::write(out_path, merged) {
                            warn!("Failed to append verify wishlist to {}: {}", out_path.display(), e);
                        } else {
                            info!("Tool wishlist (verify phase) appended to {}", out_path.display());
                        }
                    }
                    (Ok(new_entries), Err(_)) => {
                        // No translate-phase file yet — write fresh.
                        if let Err(e) = fs::write(out_path, new_entries) {
                            warn!("Failed to write verify wishlist to {}: {}", out_path.display(), e);
                        } else {
                            info!("Tool wishlist written to {}", out_path.display());
                        }
                    }
                    (Err(e), _) => {
                        warn!("Failed to read local wishlist {}: {}", local_wishlist.display(), e);
                    }
                }
            }
        }

        // Copy verify-phase HYPOTHESES.md out before the tempdir is dropped.
        // The agent maintains this as an append-only log of bug hypotheses across
        // its own compactions; absence is normal if no bugs needed investigation.
        let local_hypotheses = translated.join("HYPOTHESES.md");
        if local_hypotheses.exists() {
            if let Some(out_path) = &config.hypotheses_output_path {
                if let Err(e) = fs::copy(&local_hypotheses, out_path) {
                    warn!("Failed to copy HYPOTHESES.md to {}: {}", out_path.display(), e);
                } else {
                    info!("Verification HYPOTHESES.md written to {}", out_path.display());
                }
            }
        } else {
            info!("Agent did not produce a HYPOTHESES.md (no bugs investigated, or skipped step 1)");
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

/// Loads the verify prompt for the given agent kind.
fn load_verify_prompt(
    config: &Config,
    agent: AgentKind,
) -> Result<String, Box<dyn std::error::Error>> {
    match agent {
        AgentKind::Claude => match &config.prompt_claude_verify {
            Some(p) => Ok(fs::read_to_string(p)?),
            None if config.no_plan => Ok(PROMPT_CLAUDE_VERIFY_NO_PLAN.to_owned()),
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
    model: Option<&str>,
    no_plan: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(
        "Invoking verification agent ({agent}, model={}, no_plan={no_plan}, timeout={timeout_secs}s)",
        model.unwrap_or("(cli default)")
    );

    let logs_dir = work_dir.parent().unwrap_or(work_dir).join("logs");
    fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join("verify.log");
    let openssl_dir = std::env::var("OPENSSL_DIR").unwrap_or_else(|_| "/usr".into());

    let model_flag = model.map(|_| "--model \"$MODEL\" ").unwrap_or_default();
    let append_sys_flag = if no_plan { "" } else { "--append-system-prompt \"$APPEND_SYS\" " };

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
        AgentKind::Claude => {
            let mut cmd = Command::new("bash");
            cmd.arg("-c")
                .arg(format!(
                    "set -o pipefail; timeout {timeout_secs} claude -p \"$PROMPT\" \
                     {model_flag}\
                     --allowedTools 'Bash(*)' 'Write' 'Edit' \
                     {append_sys_flag}\
                     --output-format stream-json --verbose \
                     < /dev/null 2>&1 | tee \"$LOG\"",
                ))
                .env("PROMPT", prompt)
                .env("LOG", &log_path)
                .env("OPENSSL_DIR", &openssl_dir)
                .current_dir(work_dir);
            if !no_plan {
                cmd.env(
                    "APPEND_SYS",
                    "After any context compaction, you MUST first read PLAN.md and HYPOTHESES.md.",
                );
            }
            if let Some(m) = model {
                cmd.env("MODEL", m);
            }
            cmd.status()?
        }
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
/// Render `TestConfig` as the markdown block that the verify prompt expects in
/// place of `{ALL_CONFIGURATIONS}`. Empty when there are no project knobs to
/// switch — in that case the conditional "If configurations are listed below"
/// section silently no-ops.
fn render_configurations(cfg: &TestConfig) -> String {
    if cfg.is_empty() {
        return String::new();
    }
    format!("Configurations to verify:\n{}", cfg.as_markdown_bullet())
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

    /// Claude model to use. If absent, no --model flag is passed and the CLI uses its default.
    /// Accepts short aliases ("sonnet", "opus", "haiku") or full model IDs.
    pub model: Option<String>,

    /// If true, use the pre-anti-compaction prompt (no HYPOTHESES.md / Invariants
    /// / sub-agent push) and skip the `--append-system-prompt` flag. Intended
    /// for controlled experiments measuring the impact of the anti-compaction
    /// mechanism added in 883e2e2.
    #[serde(default)]
    pub no_plan: bool,


    /// Destination path for the agent's tool wishlist file.
    /// Injected by the benchmark at runtime (set to <output_dir>/tool_wishlist.json).
    /// Verify-phase entries are appended to any existing translate-phase entries at this path.
    /// If absent, any wishlist the agent writes is silently discarded with the tempdir.
    pub wishlist_output_path: Option<PathBuf>,

    /// Destination path for the agent's verify-phase HYPOTHESES.md.
    /// Injected by the benchmark at runtime (set to <output_dir>/hypotheses_verify.md).
    /// If absent, any hypotheses log the agent writes is preserved only inside the
    /// CargoPackage and may be lost in downstream tooling.
    pub hypotheses_output_path: Option<PathBuf>,

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
