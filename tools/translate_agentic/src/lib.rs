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
use harvest_core::cargo_utils::{CargoToml, sanitize_package_name, strip_for_lib};
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
const PROMPT_CLAUDE_TRANSLATE: &str = include_str!("prompt_claude_translate.md");

/// Agent-only structural notes appended after the variable-listing section
/// when `BuildConfigIR.is_empty == false`. Says only the things that are
/// specific to the agentic translation path -- everything mechanical about
/// the variables themselves comes from `build_system_prompt`.
const AGENTIC_CONSTRAINTS: &str = "\n\
**Notes for the agentic path**:\n\
- Do NOT write a `build.rs` -- `EmitBuildFeatures` generates it after you finish.\n\
- Source files that are selected per-variant (e.g. `backend_${BACKEND}.c`) \
  should each be translated into a separate Rust module annotated with the \
  matching `#[cfg(...)]` rule above.\n\
\n\
**Sub-task decomposition** -- this is a large, configurable project. \
Do NOT try to translate everything in one go. Instead:\n\
1. Analyze the C project structure and create a plan breaking the translation \
   into subtasks (e.g., core/shared code, each backend, entry points).\n\
2. For each subtask, invoke a subagent by running:\n\
   ```\n\
   kiro-cli chat --no-interactive --trust-all-tools '<detailed prompt>' < /dev/null\n\
   ```\n\
3. After each subagent completes, verify the work compiles before moving on.\n\
4. Once all subtasks are done, wire up the feature gates and verify the full build.\n\
\n\
Each subagent must work in the same directory. Give each a clear, focused prompt \
with the specific C files to translate and where to write the Rust output. \
Each subagent prompt MUST include:\n\
- Which specific C source files to translate.\n\
- Which Rust file(s) to write.\n\
- Instructions to build and verify its own work compiles with the relevant features.\n\
- Instructions to NOT modify any files outside its scope.\n\
\n\
After all subagents complete, wire up the feature gates and do a final build check. \
If a combination fails, only fix the glue code (lib.rs, mod declarations) -- \
do NOT modify the backend implementation files.\n";

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

        let agent = context.config.agentic_agent;
        let translate_prompt = load_prompt(
            &config.prompt_executable,
            &config.prompt_library,
            &config.prompt_claude_translate,
            &project_spec.kind,
            build_cfg,
            agent,
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

        // Pre-generate a deterministic project scaffold (Cargo.toml with the
        // canonical crate name, build.rs, rust-toolchain.toml) so the agent
        // writes its sources against the final crate name from the start. This
        // avoids the post-hoc `normalize_name` rename in `try_cargo_build`
        // breaking the agent's `use <crate>::` imports.
        let canonical_name = canonical_crate_name(&context.config.output);
        write_project_scaffold(&translated, &canonical_name, &project_spec.kind, build_cfg)?;

        invoke_agent(&translated, &translate_prompt, config.timeout_secs, agent)?;

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

/// Invokes the translation agent in `work_dir` with the given prompt and
/// timeout. The shell command differs per [`AgentKind`]: Kiro invokes
/// `kiro-cli chat`; Claude invokes `claude -p`.
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
                 --permission-mode bypassPermissions \
                 --allowedTools 'Bash(*)' 'Write' 'Edit' \
                 --append-system-prompt \"$APPEND_SYS\" \
                 --output-format stream-json --verbose \
                 < /dev/null 2>&1 | tee \"$LOG\"",
            ))
            .env("PROMPT", prompt)
            .env("LOG", &log_path)
            // Survives context compaction: the lossy summary retains this line,
            // so the agent re-reads PLAN.md (its durable plan) before resuming.
            .env(
                "APPEND_SYS",
                "After any context compaction, you MUST re-read PLAN.md (if it exists) \
                 before doing anything else, and resume from the first unchecked subtask.",
            )
            .env("OPENSSL_DIR", &openssl_dir)
            .current_dir(work_dir)
            .status()?,
    };

    if !status.success() {
        warn!("Translation agent exited with {status}");
    }
    Ok(())
}

/// Pinned Rust toolchain for the generated project, so it builds reproducibly
/// rather than against whatever toolchain happens to be the default.
const RUST_TOOLCHAIN_TOML: &str = "[toolchain]\nchannel = \"1.93.0\"\n";

/// The canonical crate name for the translated project: the sanitized basename
/// of the output directory. This matches what `try_cargo_build`'s
/// `normalize_name` would later impose, so handing it to the agent up front
/// keeps the agent's `use <crate>::` imports consistent with the final name.
fn canonical_crate_name(output: &Path) -> String {
    let raw = output.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let name = sanitize_package_name(raw);
    if name.is_empty() {
        "translated_crate".to_string()
    } else {
        name
    }
}

/// Writes a deterministic project scaffold into `dir`: a `Cargo.toml` carrying
/// the canonical package/lib name, the `[lib]`/`[[bin]]` section for the
/// [`ProjectKind`], and the configurable-variables `[features]`; a `build.rs`
/// when the config needs one; and a pinned `rust-toolchain.toml`. The agent is
/// told to preserve these and write only `src/`.
fn write_project_scaffold(
    dir: &Path,
    canonical_name: &str,
    kind: &ProjectKind,
    build_cfg: &BuildConfigIR,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = format!(
        "[package]\nname = \"{canonical_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
    );
    let mut cargo = CargoToml::from_bytes(base.as_bytes())?;
    match kind {
        ProjectKind::Library => cargo.set_lib(canonical_name),
        ProjectKind::Executable => cargo.set_bin_driver(),
    }
    cargo.add_workspace();
    let build_rs = emit_build_features::apply_features_to_cargo(&mut cargo, build_cfg);

    fs::write(dir.join("Cargo.toml"), cargo.into_bytes())?;
    if let Some(rendered) = build_rs {
        fs::write(dir.join("build.rs"), rendered)?;
    }
    fs::write(dir.join("rust-toolchain.toml"), RUST_TOOLCHAIN_TOML)?;
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

/// Loads the translate prompt.
///
/// Selection depends on [`AgentKind`]:
///
/// - [`AgentKind::Kiro`] (default): the per-kind built-in prompt
///   (`prompt_executable.md` / `prompt_library.md`) is the base; on a
///   non-empty `BuildConfigIR` it is extended with the shared
///   configurable-variables section from [`build_system_prompt`] and the
///   Kiro-specific [`AGENTIC_CONSTRAINTS`].
/// - [`AgentKind::Claude`]: the unified `prompt_claude_translate.md` is the
///   base. It already documents the configurability rules inline, so it is
///   returned as-is on the empty-IR path. On a non-empty IR the shared
///   variable-listing section from [`build_system_prompt`] is appended so
///   the agent sees the concrete variable inventory; the Kiro-only
///   sub-task-decomposition constraints are not appended (they reference
///   `kiro-cli` invocations).
///
/// Tool-config path overrides take precedence within each agent's branch:
/// `prompt_executable` / `prompt_library` for Kiro, `prompt_claude_translate`
/// for Claude. When supplied, the override file is returned verbatim and
/// the caller is responsible for any configurable-variables wording.
fn load_prompt(
    prompt_executable: &Option<PathBuf>,
    prompt_library: &Option<PathBuf>,
    prompt_claude_translate: &Option<PathBuf>,
    kind: &ProjectKind,
    build_cfg: &BuildConfigIR,
    agent: AgentKind,
) -> Result<String, Box<dyn std::error::Error>> {
    match agent {
        AgentKind::Kiro => {
            let (config_path, legacy) = match kind {
                ProjectKind::Executable => (prompt_executable, PROMPT_EXECUTABLE),
                ProjectKind::Library => (prompt_library, PROMPT_LIBRARY),
            };
            if let Some(p) = config_path {
                return Ok(fs::read_to_string(p)?);
            }
            if build_cfg.is_empty {
                return Ok(legacy.to_owned());
            }
            let with_vars = build_system_prompt(legacy, build_cfg);
            Ok(format!("{with_vars}{AGENTIC_CONSTRAINTS}"))
        }
        AgentKind::Claude => {
            if let Some(p) = prompt_claude_translate {
                return Ok(fs::read_to_string(p)?);
            }
            if build_cfg.is_empty {
                return Ok(PROMPT_CLAUDE_TRANSLATE.to_owned());
            }
            Ok(build_system_prompt(PROMPT_CLAUDE_TRANSLATE, build_cfg))
        }
    }
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
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Executable,
            &empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
        assert_eq!(
            prompt, PROMPT_EXECUTABLE,
            "empty IR must return legacy executable prompt byte-for-byte"
        );
    }

    /// With an empty IR, the library path returns the legacy prompt verbatim.
    #[test]
    fn load_prompt_empty_ir_library_returns_legacy() {
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Library,
            &empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
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
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Executable,
            &non_empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
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
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Library,
            &non_empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
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
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Executable,
            &ir,
            AgentKind::Kiro,
        )
        .unwrap();
        assert!(prompt.contains("#[cfg(feature = \"ENABLE_EXTRA\")]"));
        assert!(!prompt.contains("#[cfg(feature = \"enable_extra\")]"));
    }

    /// Non-empty IR includes sub-task decomposition guidance (kiro-cli invocation).
    #[test]
    fn load_prompt_non_empty_ir_includes_subtask_decomposition() {
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Executable,
            &non_empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
        assert!(
            prompt.contains("kiro-cli chat --no-interactive --trust-all-tools"),
            "agentic constraints must include kiro-cli subagent invocation"
        );
        assert!(
            prompt.contains("Sub-task decomposition"),
            "agentic constraints must include sub-task decomposition section"
        );
        // Must instruct each subagent not to modify files outside its scope.
        assert!(prompt.contains("NOT modify any files outside its scope"));
    }

    /// Empty IR must NOT include the sub-task decomposition guidance.
    #[test]
    fn load_prompt_empty_ir_omits_subtask_decomposition() {
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Executable,
            &empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
        assert!(
            !prompt.contains("kiro-cli chat --no-interactive --trust-all-tools"),
            "empty IR must not include kiro-cli guidance"
        );
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
            &None,
            &ProjectKind::Executable,
            &non_empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
        assert_eq!(prompt, "CUSTOM_PROMPT");

        // Same override with empty IR -- still verbatim.
        let prompt2 = load_prompt(
            &Some(p),
            &None,
            &None,
            &ProjectKind::Executable,
            &empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
        assert_eq!(prompt2, "CUSTOM_PROMPT");
    }

    // ---------- Claude agent path ----------

    /// Claude path with empty IR returns the unified Claude prompt verbatim,
    /// regardless of the project kind. The Kiro per-kind prompts are not
    /// touched.
    #[test]
    fn load_prompt_claude_empty_ir_returns_claude_prompt() {
        for kind in [ProjectKind::Executable, ProjectKind::Library] {
            let prompt =
                load_prompt(&None, &None, &None, &kind, &empty_ir(), AgentKind::Claude).unwrap();
            assert_eq!(
                prompt, PROMPT_CLAUDE_TRANSLATE,
                "claude empty-IR must return the unified prompt byte-for-byte ({kind})",
            );
        }
    }

    /// Claude path with non-empty IR appends the configurable-variables
    /// section from `build_system_prompt`. The Kiro-only sub-task
    /// decomposition (kiro-cli references) is not appended.
    #[test]
    fn load_prompt_claude_non_empty_ir_extends_with_vars_only() {
        let prompt = load_prompt(
            &None,
            &None,
            &None,
            &ProjectKind::Executable,
            &non_empty_ir(),
            AgentKind::Claude,
        )
        .unwrap();
        assert!(
            prompt.starts_with(PROMPT_CLAUDE_TRANSLATE),
            "claude non-empty prompt must start with the unified base",
        );
        assert!(prompt.contains("## Configurable variables"));
        assert!(prompt.contains("#[cfg(BACKEND_alpha)]"));
        assert!(
            !prompt.contains("kiro-cli chat --no-interactive --trust-all-tools"),
            "claude path must not include kiro-cli sub-task decomposition",
        );
    }

    /// The Claude config-path override is honored verbatim regardless of IR.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn load_prompt_claude_config_override_is_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("custom_claude.md");
        fs::write(&p, b"CUSTOM_CLAUDE_PROMPT").unwrap();
        let prompt = load_prompt(
            &None,
            &None,
            &Some(p.clone()),
            &ProjectKind::Library,
            &non_empty_ir(),
            AgentKind::Claude,
        )
        .unwrap();
        assert_eq!(prompt, "CUSTOM_CLAUDE_PROMPT");

        let prompt2 = load_prompt(
            &None,
            &None,
            &Some(p),
            &ProjectKind::Library,
            &empty_ir(),
            AgentKind::Claude,
        )
        .unwrap();
        assert_eq!(prompt2, "CUSTOM_CLAUDE_PROMPT");
    }

    /// The Kiro `prompt_claude_translate` config field is ignored on the
    /// Kiro path, and vice versa.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn load_prompt_kiro_ignores_claude_override() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("claude.md");
        fs::write(&p, b"CLAUDE_OVERRIDE").unwrap();
        let prompt = load_prompt(
            &None,
            &None,
            &Some(p),
            &ProjectKind::Executable,
            &empty_ir(),
            AgentKind::Kiro,
        )
        .unwrap();
        // Kiro path returns the legacy executable prompt; the Claude-only
        // override is not consulted.
        assert_eq!(prompt, PROMPT_EXECUTABLE);
    }

    /// The Claude prompt asserts the corrected case-preserving rule -- not the
    /// milestone3 "lowercase" wording -- and defers the `[features]` block to the
    /// pre-generated scaffold rather than asking the agent to author it.
    #[test]
    fn claude_prompt_documents_case_preserving_features() {
        assert!(
            !PROMPT_CLAUDE_TRANSLATE.contains("exact same name in lowercase"),
            "the milestone3 lowercase rule was wrong and must not survive in the ported prompt",
        );
        assert!(
            PROMPT_CLAUDE_TRANSLATE.contains("are already written for you"),
            "the Claude prompt should state the [features] block is provided by the scaffold",
        );
        // Anti-compaction methodology must be present.
        assert!(
            PROMPT_CLAUDE_TRANSLATE.contains("PLAN.md"),
            "the Claude prompt should describe the PLAN.md anti-compaction mechanism",
        );
    }
}

/// Tool-specific configuration, read from `[tools.translate_agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Override path for the executable translation prompt.
    pub prompt_executable: Option<PathBuf>,

    /// Override path for the library translation prompt.
    pub prompt_library: Option<PathBuf>,

    /// Override path for the Claude unified translation prompt. Only used
    /// when `core::config::Config::agentic_agent == AgentKind::Claude`.
    pub prompt_claude_translate: Option<PathBuf>,

    /// Agent timeout in seconds. Defaults to 7200 (2 hours). A full agentic
    /// translation of a large project can legitimately take an hour or more, so
    /// this is generous; override it in `[tools.translate_agentic]` if needed.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

fn default_timeout_secs() -> u64 {
    7200
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.translate_agentic", &self.unknown);
    }
}
