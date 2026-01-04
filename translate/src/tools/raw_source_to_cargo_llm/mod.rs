//! Attempts to directly turn a C project into a Cargo project by throwing it at
//! an LLM via the `llm` crate.

use crate::cli::unknown_field_warning;
use crate::load_raw_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation, fs::RawDir};
use llm::builder::{LLMBackend, LLMBuilder};
use llm::chat::{ChatMessage, StructuredOutputFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tracing::{debug, info, trace};

use super::identify_project_kind::ProjectKind;

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
        let config = &context.config.tools.raw_source_to_cargo_llm;
        log::debug!("LLM Configuration {config:?}");
        // Get both inputs: RawSource and ProjectKind
        let in_dir = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;
        let project_kind = context
            .ir_snapshot
            .get::<ProjectKind>(inputs[1])
            .ok_or("No ProjectKind representation found in IR")?;

        // Use the llm crate to connect to Ollama.
        let output_format: StructuredOutputFormat = serde_json::from_str(STRUCTURED_OUTPUT_SCHEMA)?;

        // TODO: This is a workaround for a flaw in the current
        // version (1.3.4) of the `llm` crate. While it supports
        // OpenRouter, the `openrouter` variant hadn't been added to
        // `from_str`. It's fixed on git tip, but not in a release
        // version. So just check for that case explicitly.
        let backend = if config.backend == "openrouter" {
            LLMBackend::OpenRouter
        } else {
            LLMBackend::from_str(&config.backend).expect("unknown LLM_BACKEND")
        };
        let llm = {
            let mut llm_builder = LLMBuilder::new()
                .backend(backend)
                .model(&config.model)
                .max_tokens(config.max_tokens)
                .temperature(0.0) // Suggestion from https://ollama.com/blog/structured-outputs
                .schema(output_format);

            match project_kind {
                ProjectKind::Executable => {
                    llm_builder = llm_builder.system(SYSTEM_PROMPT_EXECUTABLE);
                }
                ProjectKind::Library => {
                    llm_builder = llm_builder.system(SYSTEM_PROMPT_LIBRARY);
                }
            }

            if let Some(ref address) = config.address
                && !address.is_empty()
            {
                llm_builder = llm_builder.base_url(address);
            }
            if let Some(ref api_key) = config.api_key
                && !api_key.0.is_empty()
            {
                llm_builder = llm_builder.api_key(&api_key.0);
            }

            llm_builder.build().expect("Failed to build LLM (Ollama)")
        };

        // Assemble the Ollama request.
        let mut request = vec!["Please translate the following C project into a Rust project including Cargo manifest:".into()];
        request.push(
            serde_json::json!({"files": (&in_dir.dir.files_recursive().iter().map(|(path, contents)| {
                OutputFile {
                    path: path.clone(),
                    contents: String::from_utf8_lossy(contents).into(),
                }
        }).collect::<Vec<OutputFile>>())})
            .to_string(),
        );
        // "return as JSON" is suggested by https://ollama.com/blog/structured-outputs
        request.push("return as JSON".into());
        let request: Vec<_> = request
            .iter()
            .map(|contents| ChatMessage::user().content(contents).build())
            .collect();

        // Make the LLM call.
        trace!("Making LLM call with {:?}", request);
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("tokio failed")
            .block_on(llm.chat(&request))?
            .text()
            .expect("no response text");

        // Parse the response, convert it into a CargoPackage representation.
        #[derive(Deserialize)]
        struct OutputFiles {
            files: Vec<OutputFile>,
        }
        let response = response.strip_prefix("```").unwrap_or(&response);
        let response = response.strip_prefix("json").unwrap_or(response);
        let response = response.strip_suffix("```").unwrap_or(response);
        trace!("LLM responded: {:?}", &response);
        let files: OutputFiles = serde_json::from_str(response)?;
        info!("LLM response contains {} files.", files.files.len());
        let mut out_dir = RawDir::default();
        for file in files.files {
            out_dir.set_file(&file.path, file.contents.into())?;
        }
        Ok(Box::new(CargoPackage { dir: out_dir }))
    }
}

/// A cargo project representation (Cargo.toml, src/, etc).
pub struct CargoPackage {
    pub dir: RawDir,
}

impl std::fmt::Display for CargoPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Cargo package:")?;
        self.dir.display(0, f)
    }
}

impl Representation for CargoPackage {
    fn name(&self) -> &'static str {
        "CargoPackage"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        self.dir.materialize(path)
    }
}

#[derive(Deserialize)]
pub struct ApiKey(String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("********")
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Hostname and port at which to find the LLM serve. Example: "http://[::1]:11434"
    address: Option<String>,

    /// API Key for the LLM service.
    api_key: Option<ApiKey>,

    /// Which backend to use, e.g. "ollama".
    pub backend: String,

    /// Name of the model to invoke.
    pub model: String,

    /// Maximum output tokens.
    pub max_tokens: u32,

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
            address: None,
            api_key: None,
            backend: "mock_llm".into(),
            model: "mock_model".into(),
            max_tokens: 1000,
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
