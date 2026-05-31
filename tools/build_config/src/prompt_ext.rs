//! Helpers that extend LLM system prompts with a configurable-variables
//! section derived from a [`BuildConfigIR`].
//!
//! This module is shared by every translation path that needs to teach an LLM
//! about the project's configurable variables. The extension is appended only
//! when [`BuildConfigIR::is_empty`] is `false`, so all existing TRACTOR corpus
//! projects (which have no `configuration.json`) continue to receive byte-
//! identical prompts.

use crate::ir::{BuildConfigIR, ConfigVarKind, DefineKind};

/// Builds the final system prompt string by appending a configurable-variables
/// section to `base_prompt` when `build_cfg.is_empty == false`.
///
/// # Anti-regression invariant
///
/// When `build_cfg.is_empty == true`, the returned `String` is byte-equal to
/// `base_prompt`.  Tests in each consuming crate assert this so that no-config
/// prompts can never silently drift.
pub fn build_system_prompt(base_prompt: &str, build_cfg: &BuildConfigIR) -> String {
    if build_cfg.is_empty {
        return base_prompt.to_owned();
    }
    let extension = build_configurable_vars_section(build_cfg);
    format!("{base_prompt}\n\n{extension}")
}

/// Renders the configurable-variables section that is appended to a system
/// prompt when the project carries a non-empty [`BuildConfigIR`].
pub fn build_configurable_vars_section(cfg: &BuildConfigIR) -> String {
    let mut out = String::new();

    out.push_str("## Configurable variables\n\n");
    out.push_str(
        "This project uses configurable variables managed by the build system. \
        `EmitBuildFeatures` has already written the `[features]` block and `build.rs` for you -- \
        do NOT write a `[features]` block in the Cargo.toml you produce.\n\n",
    );

    out.push_str(
        "Apply the following rules when translating `#ifdef`/`#if` guards that test \
        these variables:\n\n",
    );

    for var in &cfg.variables {
        match &var.kind {
            ConfigVarKind::Boolean => {
                out.push_str(&format!(
                    "- `{name}` (boolean): use `#[cfg(feature = \"{name}\")]`\n",
                    name = var.name
                ));
            }
            ConfigVarKind::Enum { values, .. } => {
                let variants: Vec<String> = values
                    .iter()
                    .map(|v| format!("`#[cfg({name}_{v})]`", name = var.name))
                    .collect();
                out.push_str(&format!(
                    "- `{name}` (enum, values: {values}): use bare cfg -- {variants} \
                    (NOT `feature = ...`)\n",
                    name = var.name,
                    values = values.join(", "),
                    variants = variants.join(" / "),
                ));
            }
        }
    }

    if !cfg.defines.is_empty() {
        out.push('\n');
        out.push_str(
            "Composed or bare defines are emitted by `build.rs` as `cargo:rustc-env` or \
            `cargo:rustc-cfg` lines. Access them in Rust source via `env!(\"NAME\")` for \
            string-valued defines, or via `#[cfg(NAME_value)]` for cfg-valued defines:\n\n",
        );
        for def in &cfg.defines {
            match &def.kind {
                DefineKind::QuotedString { var } | DefineKind::Bare { var } => {
                    out.push_str(&format!(
                        "- `{cname}` (from `{var}`): use `env!(\"{cname}\")`\n",
                        cname = def.c_name,
                        var = var
                    ));
                }
                DefineKind::Composed { template } => {
                    out.push_str(&format!(
                        "- `{cname}` (composed from template `{template}`): use `env!(\"{cname}\")`\n",
                        cname = def.c_name,
                        template = template
                    ));
                }
                DefineKind::GatedFlag { gate_var } => {
                    out.push_str(&format!(
                        "- `{cname}` (gated by `{gate_var}`): use `#[cfg({cname})]`\n",
                        cname = def.c_name,
                        gate_var = gate_var
                    ));
                }
            }
        }
    }

    out.push('\n');
    out.push_str(
        "## Crate hygiene\n\n\
        Do NOT use the `openssl` crate or any OpenSSL bindings. \
        Use pure-Rust crates instead (e.g., `aes` for AES-256-ECB, `sha2` for SHA-256).\n\n",
    );

    out.push_str(
        "## Linker symbol fidelity\n\n\
        C preprocessor macros can RENAME functions at the source level. \
        For example, `#define foo NAMESPACE(foo)` makes the final linker symbol \
        `PREFIX_foo`, not `foo`. \
        The Rust `#[no_mangle]` name must match the FINAL linker symbol, not the \
        source-level name. Check header files for namespace macros before \
        writing `extern \"C\"` exports.\n",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ConfigVarKind, ConfigVariable, DefineKind, DefineMapping};

    fn empty_ir() -> BuildConfigIR {
        BuildConfigIR {
            is_empty: true,
            ..Default::default()
        }
    }

    fn nonempty_ir() -> BuildConfigIR {
        BuildConfigIR {
            is_empty: false,
            variables: vec![
                ConfigVariable {
                    name: "BACKEND".to_owned(),
                    kind: ConfigVarKind::Enum {
                        values: vec!["alpha".to_owned(), "beta".to_owned()],
                        numeric: false,
                    },
                    default: Some("alpha".to_owned()),
                },
                ConfigVariable {
                    name: "ENABLE_EXTRA".to_owned(),
                    kind: ConfigVarKind::Boolean,
                    default: Some("false".to_owned()),
                },
            ],
            defines: vec![DefineMapping {
                c_name: "BUILD_PROFILE".to_owned(),
                kind: DefineKind::Composed {
                    template: "{BACKEND}_{WORD_SIZE}".to_owned(),
                },
                source_vars: vec!["BACKEND".to_owned(), "WORD_SIZE".to_owned()],
            }],
            ..Default::default()
        }
    }

    const BASE_PROMPT: &str = "You are a translator. Translate C to Rust.";

    /// Anti-regression: empty IR must return the base prompt byte-for-byte.
    #[test]
    fn empty_ir_returns_base_unchanged() {
        let result = build_system_prompt(BASE_PROMPT, &empty_ir());
        assert_eq!(result, BASE_PROMPT);
    }

    /// Non-empty IR: must extend the base prompt.
    #[test]
    fn nonempty_ir_extends_base() {
        let ir = nonempty_ir();
        let result = build_system_prompt(BASE_PROMPT, &ir);
        assert!(result.starts_with(BASE_PROMPT));
        assert!(result.len() > BASE_PROMPT.len());
    }

    /// Section header must be present.
    #[test]
    fn nonempty_ir_has_section_header() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("## Configurable variables"));
    }

    /// Enum variable: bare cfg rule present, NOT feature = "...".
    #[test]
    fn enum_var_uses_bare_cfg() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("#[cfg(BACKEND_alpha)]"));
        assert!(result.contains("#[cfg(BACKEND_beta)]"));
        assert!(!result.contains("feature = \"BACKEND_"));
    }

    /// Boolean variable: feature cfg rule.
    #[test]
    fn boolean_var_uses_feature_cfg() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("#[cfg(feature = \"ENABLE_EXTRA\")]"));
    }

    /// Composed define: env!() rule.
    #[test]
    fn composed_define_uses_env() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("env!(\"BUILD_PROFILE\")"));
    }

    /// Must instruct the LLM not to write a [features] block.
    #[test]
    fn no_features_block_instruction() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("do NOT write a `[features]` block"));
    }

    /// Crate hygiene section must be present and forbid openssl.
    #[test]
    fn crate_hygiene_section_present() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("## Crate hygiene"));
        assert!(result.contains("Do NOT use the `openssl` crate"));
        assert!(result.contains("`aes`"));
        assert!(result.contains("`sha2`"));
    }

    /// Linker symbol fidelity section must be present with namespace-macro guidance.
    #[test]
    fn linker_symbol_fidelity_section_present() {
        let result = build_system_prompt(BASE_PROMPT, &nonempty_ir());
        assert!(result.contains("## Linker symbol fidelity"));
        assert!(result.contains("FINAL linker symbol"));
        assert!(result.contains("namespace macros"));
    }

    /// Empty IR must not include the new sections.
    #[test]
    fn empty_ir_omits_strategy_sections() {
        let result = build_system_prompt(BASE_PROMPT, &empty_ir());
        assert!(!result.contains("## Crate hygiene"));
        assert!(!result.contains("## Linker symbol fidelity"));
    }
}
