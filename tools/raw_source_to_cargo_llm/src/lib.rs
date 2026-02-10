//! Attempts to directly turn a C project into a Cargo project by throwing it at
//! an LLM via the `llm` crate.

use full_source::{CargoPackage, RawSource};
use harvest_core::config::unknown_field_warning;
use harvest_core::fs::RawDir;
use harvest_core::llm::{HarvestLLM, LLMConfig, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::read_to_string;
use std::path::{Path, PathBuf};
use tracing::{debug, info, trace};

use identify_project_kind::ProjectKind;

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
        // Get both inputs: RawSource and ProjectKind
        let in_dir = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;
        let project_kind = context
            .ir_snapshot
            .get::<ProjectKind>(inputs[1])
            .ok_or("No ProjectKind representation found in IR")?;

        // Use the llm crate to connect to the LLM.
        // Select system prompt based on project kind
        let (config_prompt, builtin_prompt) = match project_kind {
            ProjectKind::Executable => (config.prompt_executable, SYSTEM_PROMPT_EXECUTABLE),
            ProjectKind::Library => (config.prompt_library, SYSTEM_PROMPT_LIBRARY),
        };
        let system_prompt = config_prompt
            .map(read_to_string)
            .transpose()?
            .unwrap_or_else(|| builtin_prompt.to_owned());
        let system_prompt = if config.header_light {
            const HEADER_LIGHT_HINT: &str = "Headers are provided only for reference. Translate the .c file; only translate header content actually used by the .c (inline functions, macros it depends on). Unused declarations without bodies do not need Rust equivalents.";
            format!(
                "{HEADER_LIGHT_HINT}\n\n{base}",
                base = system_prompt.trim_end()
            )
        } else {
            system_prompt
        };

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
        struct RequestBody {
            files: Vec<OutputFile>,
        }

        let request = build_request(
            "Please translate the following C project into a Rust project including Cargo manifest:",
            &RequestBody { files },
        )?;

        // Make the LLM call.
        trace!("Making LLM call with {:?}", request);
        let response = llm.invoke(&request)?;

        // Parse the response, convert it into a CargoPackage representation.
        #[derive(Deserialize)]
        struct OutputFiles {
            files: Vec<OutputFile>,
        }
        trace!("LLM responded: {:?}", &response);
        let files: OutputFiles = serde_json::from_str(&response)?;
        info!("LLM response contains {} files.", files.files.len());
        let mut out_dir = RawDir::default();
        let filtered = if let Some(ref single_path) = config.single_out_path {
            // Prefer file whose filename matches the target; else first non-TOML; else first.
            let target_name = Path::new(single_path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            let mut name_match: Option<OutputFile> = None;
            let mut non_toml: Option<OutputFile> = None;
            let mut first: Option<OutputFile> = None;
            for f in files.files {
                if first.is_none() {
                    first = Some(f.clone());
                }
                let is_toml = Path::new(&f.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("toml"))
                    .unwrap_or(false);
                if !is_toml && non_toml.is_none() {
                    non_toml = Some(f.clone());
                }
                if let Some(tn) = &target_name {
                    if Path::new(&f.path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n == tn)
                        .unwrap_or(false)
                    {
                        name_match = Some(f.clone());
                        break;
                    }
                }
            }
            let mut chosen = name_match
                .or(non_toml)
                .or(first)
                .ok_or("LLM returned no files")?;
            chosen.path = single_path.clone().into();
            vec![chosen]
        } else {
            files.files
        };
        for file in filtered {
            out_dir.set_file(&file.path, file.contents.into())?;
        }
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

    /// If true, headers are reference-only: translate the .c file; include only used inline/macro parts.
    #[serde(default)]
    pub header_light: bool,

    /// When set, keep only one output file and force its path to this value (used by compile_commands mode).
    #[serde(default)]
    pub single_out_path: Option<String>,

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
            },
            prompt_executable: None,
            prompt_library: None,
            header_light: false,
            single_out_path: None,
            unknown: HashMap::new(),
        }
    }
}

/// Structure representing a file created by the LLM.
#[derive(Debug, Deserialize, Serialize, Clone)]
struct OutputFile {
    contents: String,
    path: PathBuf,
}
