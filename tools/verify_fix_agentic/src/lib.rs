//! Agentic verify-and-fix tool.
//!
//! After an initial translation, this tool materializes the [`CargoPackage`](full_source::CargoPackage)
//! into a fresh working directory alongside the original C source, then invokes an external agent.
//! The agent compiles and runs both the C and Rust implementations against generated test inputs,
//! compares their outputs, and iteratively fixes the Rust code until the two agree (or the agent
//! gives up). This is dynamic, execution-based verification, not a static or formal analysis.

use build_config::{BuildConfigIR, ConfigVarKind, ConfigVariable, DefineKind, DefineMapping};
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

        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("No CargoPackage representation found in IR")?;
        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[1])
            .ok_or("No RawSource representation found in IR")?;
        let build_cfg = context
            .ir_snapshot
            .get::<BuildConfigIR>(inputs[2])
            .ok_or("No BuildConfigIR representation found in IR")?;

        let agent = context.config.agentic_agent;
        let verify_prompt = match agent {
            AgentKind::Kiro => config
                .prompt_verify
                .as_ref()
                .map(fs::read_to_string)
                .transpose()?
                .unwrap_or_else(|| PROMPT_VERIFY.to_owned()),
            AgentKind::Claude => config
                .prompt_claude_verify
                .as_ref()
                .map(fs::read_to_string)
                .transpose()?
                .unwrap_or_else(|| PROMPT_CLAUDE_VERIFY.to_owned()),
        };

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

        let cmake_flags = extract_cmake_flags(case_dir, build_cfg);
        let prompt = verify_prompt
            .replace("{CASE_DIR}", &case_dir.to_string_lossy())
            .replace("{CMAKE_BUILD_FLAGS}", &cmake_flags);

        invoke_agent(case_dir, &prompt, config.timeout_secs, agent)?;
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

/// Invokes the verification agent in `work_dir` with the given prompt and
/// timeout. The shell command differs per [`AgentKind`]: Kiro invokes
/// `kiro-cli chat`; Claude invokes `claude -p`.
fn invoke_agent(
    work_dir: &Path,
    prompt: &str,
    timeout_secs: u64,
    agent: AgentKind,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Invoking verification agent ({agent}, timeout={timeout_secs}s)");

    let logs_dir = work_dir.join("logs");
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
                 --permission-mode bypassPermissions \
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

/// Derives `-D<NAME>=<value>` CMake flags to inject into the verify prompt.
///
/// When the [`BuildConfigIR`] is non-empty, flags are derived from each
/// variable's `default` value and any [`DefineMapping`] entries:
///
/// - `Enum` / `Boolean` variables -> `-D<C_NAME>=<default>` (quoted for
///   `QuotedString` define mappings, bare for `Bare` / `Composed`).
/// - If no `DefineMapping` references a variable, the variable name itself is
///   used as the C name (the common `-DVAR=value` pattern).
///
/// When the IR `is_empty`, falls back to the narrow `CMakePresets.json` reader
/// that was required for sphincs-plus. Full CMakePresets unification is
/// deferred to a follow-up change.
fn extract_cmake_flags(case_dir: &Path, build_cfg: &BuildConfigIR) -> String {
    if !build_cfg.is_empty {
        return cmake_flags_from_ir(build_cfg);
    }
    // Fallback: narrow CMakePresets.json reader (sphincs-plus path).
    cmake_flags_from_presets(case_dir)
}

/// Derive `-D` flags from a non-empty [`BuildConfigIR`] using each variable's
/// default value.
///
/// Two passes:
///
/// 1. **Top-level pass** -- for each variable that has a `default`, emit
///    `-D<C_NAME>=<default>`. The C name is resolved through
///    [`DefineMapping`] entries: if a define mapping references the variable
///    we use the mapping's `c_name` and apply the appropriate quoting;
///    otherwise we fall back to the variable name.
///
/// 2. **Subdir-variant pass** -- for each `SubdirSelection` whose
///    `driving_var` is a top-level variable with a default, locate the
///    variant whose `value` equals that default and emit a flag for each of
///    its `defines`. This keeps per-backend `-D` flags (e.g. sphincs-shape
///    `add_compile_definitions("PARAMS=...")` inside `lib/<backend>/`)
///    surfacing in the cmake invocation.
fn cmake_flags_from_ir(build_cfg: &BuildConfigIR) -> String {
    let mut flags: Vec<String> = Vec::new();

    for var in &build_cfg.variables {
        let default_value = match &var.default {
            Some(v) => v.as_str(),
            None => continue,
        };

        // Find the first DefineMapping that references this variable.
        let define_for_var = build_cfg
            .defines
            .iter()
            .find(|d| d.source_vars.iter().any(|sv| sv == &var.name));

        match define_for_var {
            Some(dm) => match &dm.kind {
                DefineKind::QuotedString { .. } => {
                    flags.push(format!("-D{}=\"{}\"", dm.c_name, default_value));
                }
                DefineKind::Bare { .. } => {
                    flags.push(format!("-D{}={}", dm.c_name, default_value));
                }
                DefineKind::Composed { template } => {
                    // For composed defines we can only substitute a single
                    // variable's contribution; emit the variable itself.
                    let _ = template;
                    flags.push(format!("-D{}={}", var.name, default_value));
                }
                DefineKind::GatedFlag { .. } => {
                    // Gated flags are boolean; emit as VAR=default.
                    flags.push(format!("-D{}={}", var.name, default_value));
                }
            },
            // No define mapping -> use the variable name directly.
            None => match &var.kind {
                ConfigVarKind::Boolean => {
                    flags.push(format!("-D{}={}", var.name, default_value));
                }
                ConfigVarKind::Enum { .. } => {
                    flags.push(format!("-D{}={}", var.name, default_value));
                }
            },
        }
    }

    for ss in &build_cfg.subdir_selections {
        let Some(default_value) = build_cfg
            .variables
            .iter()
            .find(|v| v.name == ss.driving_var)
            .and_then(|v| v.default.as_deref())
        else {
            continue;
        };
        let Some(variant) = ss.variants.iter().find(|v| v.value == default_value) else {
            continue;
        };
        for define in &variant.defines {
            push_variant_define_flag(define, &build_cfg.variables, &mut flags);
        }
    }

    flags.join(" ")
}

/// Emit a single `-D` flag for one variant-scoped define, resolving the
/// referenced variable's default from the top-level variables list. Skips
/// defines whose source variable is unknown, has no default, or whose kind
/// is `Composed` (multi-variable substitution is not yet implemented).
fn push_variant_define_flag(
    define: &DefineMapping,
    vars: &[ConfigVariable],
    flags: &mut Vec<String>,
) {
    let Some(var_name) = define.source_vars.first() else {
        return;
    };
    let Some(var) = vars.iter().find(|v| &v.name == var_name) else {
        return;
    };
    let Some(default_value) = var.default.as_deref() else {
        return;
    };
    match &define.kind {
        DefineKind::QuotedString { .. } => {
            flags.push(format!("-D{}=\"{}\"", define.c_name, default_value));
        }
        DefineKind::Bare { .. } => {
            flags.push(format!("-D{}={}", define.c_name, default_value));
        }
        DefineKind::GatedFlag { .. } => {
            flags.push(format!("-D{}={}", define.c_name, default_value));
        }
        DefineKind::Composed { .. } => {
            // TODO: substitute all `{X}` placeholders against the variables
            // list and emit. For now, skip -- the variant.defines case we
            // care about (sphincs-shape per-backend PARAMS) is QuotedString,
            // not Composed.
        }
    }
}

/// Narrow `CMakePresets.json` reader -- the original sphincs-plus hack.
///
/// Kept as a verbatim fallback for projects whose build configuration is
/// expressed via presets rather than `configuration.json`. Full unification
/// with [`BuildConfigIR`] is deferred to a follow-up change.
fn cmake_flags_from_presets(case_dir: &Path) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use build_config::{BuildConfigIR, ConfigVarKind, ConfigVariable, DefineKind, DefineMapping};

    fn empty_ir() -> BuildConfigIR {
        BuildConfigIR {
            is_empty: true,
            ..Default::default()
        }
    }

    /// An empty IR must produce an empty string (same as presets path when no
    /// CMakePresets.json is present) -- byte-equal to current `main` on the
    /// entire existing TRACTOR corpus where `is_empty == true`.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn empty_ir_yields_empty_flags() {
        // No CMakePresets.json exists in this tempdir, so the presets fallback
        // also returns "".
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &empty_ir());
        assert_eq!(
            flags, "",
            "empty IR with no CMakePresets.json must yield empty flags"
        );
    }

    /// A non-empty IR with simple enum and boolean variables and no define
    /// mappings should emit `-DVAR=default` for each variable with a default.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn non_empty_ir_derives_flags_from_defaults() {
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![
                ConfigVariable {
                    name: "BACKEND".into(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["alpha".into(), "beta".into()],
                        numeric: false,
                    },
                    default: Some("alpha".into()),
                },
                ConfigVariable {
                    name: "WORD_SIZE".into(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["32".into(), "64".into()],
                        numeric: true,
                    },
                    default: Some("32".into()),
                },
                ConfigVariable {
                    name: "ENABLE_EXTRA".into(),
                    kind: ConfigVarKind::Boolean,
                    default: Some("false".into()),
                },
                // Variable with no default should be silently skipped.
                ConfigVariable {
                    name: "NO_DEFAULT".into(),
                    kind: ConfigVarKind::Boolean,
                    default: None,
                },
            ],
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        // All three variables with defaults must appear; order follows
        // `variables` order.
        assert!(flags.contains("-DBACKEND=alpha"), "flags={flags}");
        assert!(flags.contains("-DWORD_SIZE=32"), "flags={flags}");
        assert!(flags.contains("-DENABLE_EXTRA=false"), "flags={flags}");
        // The variable without a default must not appear.
        assert!(!flags.contains("NO_DEFAULT"), "flags={flags}");
    }

    /// When a `DefineMapping` with `Bare` kind references a variable, the
    /// C name from the mapping is used instead of the variable name.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn non_empty_ir_uses_define_mapping_c_name_for_bare() {
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![ConfigVariable {
                name: "WORD_SIZE".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["32".into(), "64".into()],
                    numeric: true,
                },
                default: Some("32".into()),
            }],
            defines: vec![DefineMapping {
                c_name: "WORD_SIZE".into(),
                kind: DefineKind::Bare {
                    var: "WORD_SIZE".into(),
                },
                source_vars: vec!["WORD_SIZE".into()],
            }],
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        assert!(flags.contains("-DWORD_SIZE=32"), "flags={flags}");
    }

    /// When a `DefineMapping` with `QuotedString` kind references a variable,
    /// the value is quoted in the emitted flag.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn non_empty_ir_uses_define_mapping_c_name_for_quoted_string() {
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![ConfigVariable {
                name: "APP_MODE".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["fast".into(), "safe".into()],
                    numeric: false,
                },
                default: Some("fast".into()),
            }],
            defines: vec![DefineMapping {
                c_name: "APP_MODE_STR".into(),
                kind: DefineKind::QuotedString {
                    var: "APP_MODE".into(),
                },
                source_vars: vec!["APP_MODE".into()],
            }],
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        assert!(flags.contains("-DAPP_MODE_STR=\"fast\""), "flags={flags}");
    }

    /// Sphincs-shape: a top-level `HASH_BACKEND` variable selects a subdir
    /// variant whose own `add_compile_definitions("PARAMS=${PARAMS_KEY}")`
    /// gets captured as a QuotedString define inside that variant. The
    /// variant matching `HASH_BACKEND`'s default contributes a `-DPARAMS="..."`
    /// flag derived from the inner-referenced variable's default.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn subdir_variant_quoted_define_yields_per_backend_flag() {
        use build_config::{SubdirSelection, SubdirVariant};
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![
                ConfigVariable {
                    name: "HASH_BACKEND".into(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["blake".into(), "sha2".into()],
                        numeric: false,
                    },
                    default: Some("blake".into()),
                },
                // The variant-scoped define references `PARAMS_KEY`, which is
                // a top-level variable whose default supplies the value.
                ConfigVariable {
                    name: "PARAMS_KEY".into(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["sphincs-blake-128s".into()],
                        numeric: false,
                    },
                    default: Some("sphincs-blake-128s".into()),
                },
            ],
            subdir_selections: vec![SubdirSelection {
                driving_var: "HASH_BACKEND".into(),
                variants: vec![SubdirVariant {
                    value: "blake".into(),
                    path: PathBuf::from("lib/blake"),
                    defines: vec![DefineMapping {
                        c_name: "PARAMS".into(),
                        kind: DefineKind::QuotedString {
                            var: "PARAMS_KEY".into(),
                        },
                        source_vars: vec!["PARAMS_KEY".into()],
                    }],
                    source_selections: Vec::new(),
                    conditional_targets: Vec::new(),
                    subdir_selections: Vec::new(),
                    targets: Vec::new(),
                }],
            }],
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        assert!(
            flags.contains("-DPARAMS=\"sphincs-blake-128s\""),
            "expected -DPARAMS=\"sphincs-blake-128s\" in flags={flags}",
        );
    }

    /// A `SubdirSelection` whose `driving_var` has no top-level default is
    /// silently skipped -- we cannot pick a variant without a default.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn subdir_selection_with_no_driving_default_is_skipped() {
        use build_config::{SubdirSelection, SubdirVariant};
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![ConfigVariable {
                name: "HASH_BACKEND".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["blake".into()],
                    numeric: false,
                },
                default: None,
            }],
            subdir_selections: vec![SubdirSelection {
                driving_var: "HASH_BACKEND".into(),
                variants: vec![SubdirVariant {
                    value: "blake".into(),
                    path: PathBuf::from("lib/blake"),
                    defines: vec![DefineMapping {
                        c_name: "PARAMS".into(),
                        kind: DefineKind::Bare {
                            var: "MISSING".into(),
                        },
                        source_vars: vec!["MISSING".into()],
                    }],
                    source_selections: Vec::new(),
                    conditional_targets: Vec::new(),
                    subdir_selections: Vec::new(),
                    targets: Vec::new(),
                }],
            }],
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        assert!(!flags.contains("PARAMS"), "flags={flags}");
    }

    /// Only the variant whose `value` matches the driving variable's default
    /// contributes flags. Non-matching variants are silently skipped, even
    /// when they would otherwise produce a valid flag.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn only_matching_subdir_variant_contributes_flags() {
        use build_config::{SubdirSelection, SubdirVariant};
        let ir = BuildConfigIR {
            is_empty: false,
            variables: vec![
                ConfigVariable {
                    name: "HASH_BACKEND".into(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["blake".into(), "sha2".into()],
                        numeric: false,
                    },
                    default: Some("blake".into()),
                },
                ConfigVariable {
                    name: "PARAMS_KEY".into(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["sphincs-blake-128s".into(), "sphincs-sha2-128s".into()],
                        numeric: false,
                    },
                    default: Some("sphincs-blake-128s".into()),
                },
            ],
            subdir_selections: vec![SubdirSelection {
                driving_var: "HASH_BACKEND".into(),
                variants: vec![
                    SubdirVariant {
                        value: "blake".into(),
                        path: PathBuf::from("lib/blake"),
                        defines: vec![DefineMapping {
                            c_name: "PARAMS".into(),
                            kind: DefineKind::QuotedString {
                                var: "PARAMS_KEY".into(),
                            },
                            source_vars: vec!["PARAMS_KEY".into()],
                        }],
                        source_selections: Vec::new(),
                        conditional_targets: Vec::new(),
                        subdir_selections: Vec::new(),
                        targets: Vec::new(),
                    },
                    SubdirVariant {
                        value: "sha2".into(),
                        path: PathBuf::from("lib/sha2"),
                        defines: vec![DefineMapping {
                            c_name: "SHOULD_NOT_APPEAR".into(),
                            kind: DefineKind::Bare {
                                var: "PARAMS_KEY".into(),
                            },
                            source_vars: vec!["PARAMS_KEY".into()],
                        }],
                        source_selections: Vec::new(),
                        conditional_targets: Vec::new(),
                        subdir_selections: Vec::new(),
                        targets: Vec::new(),
                    },
                ],
            }],
            ..Default::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        assert!(flags.contains("-DPARAMS="), "flags={flags}");
        assert!(
            !flags.contains("SHOULD_NOT_APPEAR"),
            "non-default variant must not contribute; flags={flags}",
        );
    }

    /// Anti-regression: a flat IR (no `subdir_selections`) produces the
    /// same flags as before the subdir-variant descent was added. We assert
    /// this by reusing the smallest top-level shape.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn flat_ir_unchanged_by_subdir_descent() {
        let ir = BuildConfigIR {
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
        };
        let dir = tempfile::tempdir().unwrap();
        let flags = extract_cmake_flags(dir.path(), &ir);
        assert_eq!(flags, "-DBACKEND=alpha", "flags={flags}");
    }

    /// The presets fallback is exercised when the IR is empty and a
    /// `CMakePresets.json` is present. Byte-equal to the original function.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn empty_ir_with_presets_file_falls_back_to_presets() {
        let dir = tempfile::tempdir().unwrap();
        let c_src = dir.path().join("translated_rust/c_src");
        fs::create_dir_all(&c_src).unwrap();
        let presets_json = r#"{
            "configurePresets": [
                {},
                {
                    "cacheVariables": {
                        "CMAKE_C_STANDARD": "11",
                        "CMAKE_BUILD_TYPE": "Release",
                        "SPHINCS_VARIANT": "sha2_128f"
                    }
                }
            ]
        }"#;
        fs::write(c_src.join("CMakePresets.json"), presets_json).unwrap();

        let flags = extract_cmake_flags(dir.path(), &empty_ir());
        // CMAKE_C_STANDARD and CMAKE_BUILD_TYPE are filtered out by the presets reader.
        assert!(!flags.contains("CMAKE_C_STANDARD"), "flags={flags}");
        assert!(!flags.contains("CMAKE_BUILD_TYPE"), "flags={flags}");
        assert!(
            flags.contains("-DSPHINCS_VARIANT=sha2_128f"),
            "flags={flags}"
        );
    }

    /// The Kiro and Claude verify prompts are non-empty and distinct; the
    /// Claude prompt is the libloading-based oracle comparison while the
    /// Kiro prompt is the legacy form.
    #[test]
    fn claude_and_kiro_verify_prompts_are_distinct_and_nonempty() {
        assert!(!PROMPT_VERIFY.is_empty());
        assert!(!PROMPT_CLAUDE_VERIFY.is_empty());
        assert_ne!(PROMPT_VERIFY, PROMPT_CLAUDE_VERIFY);
        // The Claude prompt is the libloading/nm-based oracle form.
        assert!(PROMPT_CLAUDE_VERIFY.contains("libloading"));
        assert!(PROMPT_CLAUDE_VERIFY.contains("nm -D"));
    }
}

/// Tool-specific configuration, read from `[tools.verify_fix_agentic]` in the HARVEST config.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Override path for the Kiro verification prompt.
    pub prompt_verify: Option<PathBuf>,

    /// Override path for the Claude verification prompt. Only used when
    /// `core::config::Config::agentic_agent == AgentKind::Claude`.
    pub prompt_claude_verify: Option<PathBuf>,

    /// Agent timeout in seconds. Defaults to 5400 (90 minutes). Override it in
    /// `[tools.verify_fix_agentic]` if needed.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

fn default_timeout_secs() -> u64 {
    5400
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.verify_fix_agentic", &self.unknown);
    }
}
