//! Attempts to directly turn a C project into a Cargo project by throwing it at
//! an LLM via the `llm` crate.

use build_config::{BuildConfigIR, build_system_prompt};
use build_project_spec::{ProjectKind, ProjectSpec};
use full_source::{CargoPackage, RawSource};
use harvest_core::config::unknown_field_warning;
use harvest_core::fs::RawDir;
use harvest_core::llm::{HarvestLLM, LLMConfig, LLMUsageTotals, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::read_to_string;
use std::path::PathBuf;
use tracing::{debug, info, trace};

/// Structured output JSON schema for Ollama.
const STRUCTURED_OUTPUT_SCHEMA: &str = include_str!("structured_schema.json");

const SYSTEM_PROMPT_EXECUTABLE: &str = include_str!("system_prompt_executable.txt");
const SYSTEM_PROMPT_LIBRARY: &str = include_str!("system_prompt_library.txt");

pub struct RawSourceToCargoLlm;

impl Tool for RawSourceToCargoLlm {
    fn name(&self) -> &'static str {
        "raw_source_to_cargo_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config =
            Config::deserialize(context.config.tools.get("raw_source_to_cargo_llm").unwrap())?;
        debug!("LLM Configuration {config:?}");
        // Get inputs: RawSource, ProjectSpec, BuildConfigIR
        let in_dir = context
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
        let project_kind = &project_spec.kind;

        // Build the system prompt programmatically:
        // 1. Start with the static base prompt (executable or library variant).
        // 2. Append a configurable-variables section only when the IR is non-empty.
        let (config_prompt, builtin_prompt) = match project_kind {
            ProjectKind::Executable => (config.prompt_executable, SYSTEM_PROMPT_EXECUTABLE),
            ProjectKind::Library => (config.prompt_library, SYSTEM_PROMPT_LIBRARY),
        };
        let base_prompt = config_prompt
            .map(read_to_string)
            .transpose()?
            .unwrap_or_else(|| builtin_prompt.to_owned());

        let system_prompt = build_system_prompt(&base_prompt, build_cfg);

        // Build LLM client using core/llm
        let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, &system_prompt)?;

        // Assemble the LLM request.
        let files: Vec<OutputFile> = in_dir
            .dir
            .files_recursive()
            .iter()
            .map(|(path, contents)| OutputFile {
                path: path.clone(),
                contents: String::from_utf8_lossy(contents).into(),
            })
            .collect();

        #[derive(Serialize)]
        struct RequestBody<'a> {
            files: Vec<OutputFile>,
            #[serde(skip_serializing_if = "Option::is_none")]
            build_config: Option<&'a BuildConfigIR>,
        }

        let build_config_field = if build_cfg.is_empty {
            None
        } else {
            Some(build_cfg)
        };

        let request = build_request(
            "Please translate the following C project into a Rust project including Cargo manifest:",
            &RequestBody {
                files,
                build_config: build_config_field,
            },
        )?;

        // Make the LLM call.
        trace!("Making LLM call with {:?}", request);
        let mut usage_totals = LLMUsageTotals::default();
        let (response, usage) = llm.invoke(&request)?;
        usage_totals.add_usage(usage.as_ref());

        // Parse the response, convert it into a CargoPackage representation.
        #[derive(Deserialize)]
        struct OutputFiles {
            files: Vec<OutputFile>,
        }
        trace!("LLM responded: {:?}", &response);
        let files: OutputFiles = serde_json::from_str(&response)?;
        info!("LLM response contains {} files.", files.files.len());
        let mut out_dir = RawDir::default();
        for file in files.files {
            out_dir.set_file(&file.path, file.contents.into())?;
        }

        info!(
            "Token usage [total] - prompt: {}, output: {}, total: {}",
            usage_totals.prompt_tokens, usage_totals.output_tokens, usage_totals.total_tokens
        );

        Ok(Box::new(CargoPackage { dir: out_dir }))
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    /// LLM configuration.
    #[serde(flatten)]
    pub llm: LLMConfig,

    /// System prompt to use for executable projects. If not specified, a built-in default prompt
    /// will be used.
    pub prompt_executable: Option<PathBuf>,

    /// System prompt to use for library projects. If not specified, a built-in default prompt will
    /// be used.
    pub prompt_library: Option<PathBuf>,

    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.raw_source_to_cargo_llm", &self.unknown);
    }

    /// Returns a mock config for testing.
    pub fn mock() -> Self {
        Self {
            llm: LLMConfig {
                address: None,
                api_key: None,
                backend: "mock_llm".into(),
                model: "mock_model".into(),
                max_tokens: 1000,
                retry_count: None,
                retry_delay_secs: None,
            },
            prompt_executable: None,
            prompt_library: None,
            unknown: HashMap::new(),
        }
    }
}

/// Structure representing a file created by the LLM.
#[derive(Debug, Deserialize, Serialize)]
struct OutputFile {
    contents: String,
    path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use build_config::{ConfigVarKind, ConfigVariable, DefineKind, DefineMapping};

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

    /// Anti-regression: when `is_empty == true`, `build_system_prompt` must
    /// return a string that is byte-equal to the static include_str! constants.
    #[test]
    fn empty_ir_prompt_is_byte_equal_to_static_executable() {
        let result = build_system_prompt(SYSTEM_PROMPT_EXECUTABLE, &empty_ir());
        assert_eq!(
            result, SYSTEM_PROMPT_EXECUTABLE,
            "empty IR must not modify the executable prompt"
        );
    }

    /// Anti-regression: same check for the library prompt.
    #[test]
    fn empty_ir_prompt_is_byte_equal_to_static_library() {
        let result = build_system_prompt(SYSTEM_PROMPT_LIBRARY, &empty_ir());
        assert_eq!(
            result, SYSTEM_PROMPT_LIBRARY,
            "empty IR must not modify the library prompt"
        );
    }

    /// Non-empty IR: prompt must contain the configurable-variables section.
    #[test]
    fn nonempty_ir_prompt_contains_cfg_rules() {
        let ir = nonempty_ir();
        let result = build_system_prompt(SYSTEM_PROMPT_EXECUTABLE, &ir);

        // Starts with the base prompt unchanged.
        assert!(result.starts_with(SYSTEM_PROMPT_EXECUTABLE));

        // Contains the section header.
        assert!(
            result.contains("## Configurable variables"),
            "prompt must contain section header"
        );

        // Enum variable: bare cfg rule.
        assert!(
            result.contains("#[cfg(BACKEND_alpha)]"),
            "prompt must mention BACKEND_alpha bare cfg"
        );
        assert!(
            result.contains("#[cfg(BACKEND_beta)]"),
            "prompt must mention BACKEND_beta bare cfg"
        );
        // Must NOT say `feature = "BACKEND_..."` for the enum.
        assert!(
            !result.contains("feature = \"BACKEND_"),
            "enum cfg rules must not use feature = ..."
        );

        // Boolean variable: feature cfg rule.
        assert!(
            result.contains("#[cfg(feature = \"ENABLE_EXTRA\")]"),
            "boolean must use feature = rule"
        );

        // Composed define: env!() rule.
        assert!(
            result.contains("env!(\"BUILD_PROFILE\")"),
            "composed define must use env!()"
        );

        // Explicit: no [features] block.
        assert!(
            result.contains("do NOT write a `[features]` block"),
            "prompt must instruct LLM not to write [features]"
        );
    }

    /// Non-empty IR: the extended prompt must be strictly longer than the base.
    #[test]
    fn nonempty_ir_prompt_is_longer_than_base() {
        let ir = nonempty_ir();
        let result = build_system_prompt(SYSTEM_PROMPT_EXECUTABLE, &ir);
        assert!(
            result.len() > SYSTEM_PROMPT_EXECUTABLE.len(),
            "non-empty IR must extend the prompt"
        );
    }
}
