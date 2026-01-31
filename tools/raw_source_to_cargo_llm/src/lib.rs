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
use std::path::PathBuf;
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
        let system_prompt = match project_kind {
            ProjectKind::Executable => SYSTEM_PROMPT_EXECUTABLE,
            ProjectKind::Library => SYSTEM_PROMPT_LIBRARY,
        };

        // Build LLM client using core/llm
        let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, system_prompt)?;

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
        for file in files.files {
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
