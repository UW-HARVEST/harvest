//! Agentic verify-and-fix tool.
//!
//! After an initial translation, this tool materializes the [`CargoPackage`](full_source::CargoPackage)
//! into a fresh working directory alongside the original C source, then invokes an external agent.
//! The agent compiles and runs both the C and Rust implementations against generated test inputs,
//! compares their outputs, and iteratively fixes the Rust code until the two agree (or the agent
//! gives up). This is dynamic, execution-based verification, not a static or formal analysis.

use agent_runner::{AgentInvocation, AgentPhase};
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
use tracing::{info, warn};

const PROMPT_KIRO_VERIFY: &str = include_str!("prompt_kiro_verify.md");
const PROMPT_VERIFY: &str = include_str!("prompt_verify.md");
const PROMPT_VERIFY_NO_PLAN: &str = include_str!("prompt_verify_no_plan.md");

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

        // Wait until the specified timestamp if --wait-until was given
        if let Some(target_ts) = config.wait_until {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if now < target_ts {
                let wait_secs = target_ts - now;
                info!("Waiting {wait_secs}s until Unix timestamp {target_ts}");
                std::thread::sleep(std::time::Duration::from_secs(wait_secs));
                info!("Wait complete, starting verification");
            } else {
                info!(
                    "Target timestamp {target_ts} already passed (now={now}), starting immediately"
                );
            }
        }

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
        let rust_toolchain_context =
            agent_runner::detect_rust_toolchain_context(&context.config.input)?;

        // Look near the original input dir; the case_dir tempdir does not contain
        // CMakePresets.json because raw_source only mirrors `test_case/` content.
        let test_config = find_test_config(&context.config.input);
        let workflow_hint = if config.workflow && agent == AgentKind::Claude {
            "You must use a workflow.\n\n".to_owned()
        } else {
            String::new()
        };
        let prompt = load_verify_prompt(&config, agent)?
            .replace("{CMAKE_BUILD_FLAGS}", &test_config.cmake_flags)
            .replace("{ALL_CONFIGURATIONS}", &render_configurations(&test_config))
            .replace("{WISHLIST_PATH}", &local_wishlist.to_string_lossy())
            .replace("{AGENT_TOOLS_SECTION}", &agent_tools_section)
            .replace(
                "{RUST_TOOLCHAIN_CONTEXT}",
                &rust_toolchain_context.prompt_block,
            )
            .replace("{WORKFLOW_HINT}", &workflow_hint)
            .replace(
                "{MODEL_LIMITS}",
                &match (agent, &config.model) {
                    (AgentKind::OpenCode, Some(model)) => {
                        let limits = agent_runner::load_opencode_model_limits(model)?;
                        agent_runner::render_model_limits_block(&limits)
                    }
                    _ => String::new(),
                },
            );

        // Kiro runs in case_dir (references translated_rust/ in prompt paths).
        // Claude and OpenCode run in translated_rust/ directly (references c_src/ and src/).
        let kiro_prompt;
        let (agent_work_dir, agent_prompt) = match agent {
            AgentKind::Kiro => {
                kiro_prompt = prompt.replace("{CASE_DIR}", &case_dir.to_string_lossy());
                (case_dir, kiro_prompt.as_str())
            }
            AgentKind::Claude | AgentKind::OpenCode => (translated.as_path(), prompt.as_str()),
        };
        agent_runner::invoke_agent(AgentInvocation {
            phase: AgentPhase::Verify,
            agent,
            work_dir: agent_work_dir,
            prompt: agent_prompt,
            timeout_secs: config.timeout_secs,
            model: config.model.as_deref(),
            no_plan: config.no_plan,
            extra_env: &config.env,
            output_log_path: config.output_log_path.as_deref(),
            rust_toolchain: Some(&rust_toolchain_context.required_version),
        })?;
        info!("Verification complete");

        // Append verify-phase wishlist entries to the translate-phase file (if any),
        // so the final output contains wishes from both phases in chronological order.
        if local_wishlist.exists() {
            if let Some(out_path) = &config.wishlist_output_path {
                match (
                    fs::read_to_string(&local_wishlist),
                    fs::read_to_string(out_path),
                ) {
                    (Ok(new_entries), Ok(existing)) => {
                        let merged = format!("{}{}", existing, new_entries);
                        if let Err(e) = fs::write(out_path, merged) {
                            warn!(
                                "Failed to append verify wishlist to {}: {}",
                                out_path.display(),
                                e
                            );
                        } else {
                            info!(
                                "Tool wishlist (verify phase) appended to {}",
                                out_path.display()
                            );
                        }
                    }
                    (Ok(new_entries), Err(_)) => {
                        // No translate-phase file yet — write fresh.
                        if let Err(e) = fs::write(out_path, new_entries) {
                            warn!(
                                "Failed to write verify wishlist to {}: {}",
                                out_path.display(),
                                e
                            );
                        } else {
                            info!("Tool wishlist written to {}", out_path.display());
                        }
                    }
                    (Err(e), _) => {
                        warn!(
                            "Failed to read local wishlist {}: {}",
                            local_wishlist.display(),
                            e
                        );
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
                    warn!(
                        "Failed to copy HYPOTHESES.md to {}: {}",
                        out_path.display(),
                        e
                    );
                } else {
                    info!(
                        "Verification HYPOTHESES.md written to {}",
                        out_path.display()
                    );
                }
            }
        } else {
            info!(
                "Agent did not produce a HYPOTHESES.md (no bugs investigated, or skipped step 1)"
            );
        }

        // Sanitize translated_rust/ before freezing it into the IR.
        // 1) Remove hidden directories/files that leaked in from the agent runtime.
        // 2) Remove known intermediate build/source artifacts.
        // 3) Surface any remaining symlinks for diagnostics.
        remove_hidden_entries(&translated)?;

        let c_src_out = translated.join("c_src");
        if c_src_out.exists() {
            if let Err(e) = fs::remove_dir_all(&c_src_out) {
                warn!(
                    "Failed to remove c_src output dir {}: {}",
                    c_src_out.display(),
                    e
                );
            }
        }
        let target_out = translated.join("target");
        if target_out.exists() {
            if let Err(e) = fs::remove_dir_all(&target_out) {
                warn!(
                    "Failed to remove target output dir {}: {}",
                    target_out.display(),
                    e
                );
            }
        }

        for entry in collect_symlinks(&translated) {
            warn!("translated_rust contains symlink: {}", entry);
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
        AgentKind::Claude | AgentKind::OpenCode => match &config.prompt_verify {
            Some(p) => Ok(fs::read_to_string(p)?),
            None if config.no_plan => Ok(PROMPT_VERIFY_NO_PLAN.to_owned()),
            None => Ok(PROMPT_VERIFY.to_owned()),
        },
        AgentKind::Kiro => match &config.prompt_kiro_verify {
            Some(p) => Ok(fs::read_to_string(p)?),
            None => Ok(PROMPT_KIRO_VERIFY.to_owned()),
        },
    }
}

/// Remove hidden entries (names starting with `.`) under `dir`, including nested ones.
/// This prevents agent-runtime artifacts like `.opencode/` from leaking into the IR.
fn remove_hidden_entries(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let hidden = name.to_string_lossy().starts_with('.');
            if hidden {
                let path = entry.path();
                if let Err(e) = fs::remove_dir_all(&path) {
                    if path.is_dir() {
                        warn!(
                            "Failed to remove hidden directory {}: {}",
                            path.display(),
                            e
                        );
                    } else if let Err(e2) = fs::remove_file(&path) {
                        warn!("Failed to remove hidden entry {}: {}", path.display(), e2);
                    }
                }
            } else if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                remove_hidden_entries(&entry.path())?;
            }
        }
    }
    Ok(())
}

/// Collects symlink paths under `dir` for debugging.
fn collect_symlinks(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(metadata) = entry.metadata() {
                if metadata.file_type().is_symlink() {
                    out.push(path.display().to_string());
                } else if metadata.is_dir() {
                    out.extend(collect_symlinks(&path));
                }
            }
        }
    }
    out
}

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
    pub prompt_kiro_verify: Option<PathBuf>,

    /// Override path for the standard verification prompt.
    #[serde(alias = "prompt_claude_verify")]
    pub prompt_verify: Option<PathBuf>,

    /// Agent timeout in seconds.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Agent model to use. If absent, no --model flag is passed and the CLI uses its default.
    /// Claude accepts short aliases ("sonnet", "opus", "haiku") or full model IDs.
    /// OpenCode expects provider/model format.
    pub model: Option<String>,

    /// If true, use the pre-anti-compaction prompt (no HYPOTHESES.md / Invariants
    /// / sub-agent push) and skip the `--append-system-prompt` flag. Intended
    /// for controlled experiments measuring the impact of the anti-compaction
    /// mechanism added in 883e2e2.
    #[serde(default)]
    pub no_plan: bool,

    /// Inject a prompt hint encouraging the agent to use dynamic workflows.
    /// Only meaningful with no_plan.
    #[serde(default)]
    pub workflow: bool,

    /// Extra environment variables to inject into the agent process.
    /// Useful for CCR provider API keys, proxy settings, etc.
    /// Defined as a TOML table under `[tools.verify_fix_agentic.env]`.
    #[serde(default)]
    pub env: HashMap<String, String>,

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

    /// Unix timestamp. If set, the verification agent will sleep until this
    /// time before starting. Used to align with the 5-hour free window reset.
    /// If the current time is already past the timestamp, verification starts
    /// immediately.
    #[serde(default)]
    pub wait_until: Option<u64>,

    /// Destination path for the benchmark's output.log file.
    /// Injected by the benchmark at runtime so the agent's full trace
    /// (JSON stream) is appended to the same log file as benchmark messages.
    pub output_log_path: Option<PathBuf>,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

fn default_timeout_secs() -> u64 {
    36000
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.verify_fix_agentic", &self.unknown);
    }
}
