//! This module provides a simplified synchronous wrapper around the `llm` crate
//! This simplifies Tools that relies on LLMs by providing a common configuration
//! and deduplicates common logic (like building requests and parsing responses).

use llm::LLMProvider;
use llm::builder::{LLMBackend, LLMBuilder};
pub use llm::chat::ChatMessage;
use llm::chat::StructuredOutputFormat;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// API Key wrapper that hides the key in debug output.
#[derive(Deserialize)]
pub struct ApiKey(pub String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("********")
    }
}

/// Configuration for LLM clients.
#[derive(Debug, Deserialize)]
pub struct LLMConfig {
    /// Hostname and port at which to find the LLM serve. Example: "http://[::1]:11434"
    pub address: Option<String>,

    /// API Key for the LLM service.
    pub api_key: Option<ApiKey>,

    /// Which backend to use, e.g. "ollama".
    pub backend: String,

    /// Name of the model to invoke.
    pub model: String,

    /// Maximum output tokens.
    pub max_tokens: u32,
}

/// Wrapper for an LLM client with helper methods.
pub struct HarvestLLM {
    client: Box<dyn LLMProvider>,
}

impl HarvestLLM {
    /// Builds an LLM client from configuration.
    pub fn build(
        config: &LLMConfig,
        output_format_json: &str,
        system_prompt: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Parse the output format from JSON string
        let output_format: StructuredOutputFormat = serde_json::from_str(output_format_json)?;
        let backend = LLMBackend::from_str(&config.backend).expect("unknown LLM_BACKEND");

        let mut llm_builder = LLMBuilder::new()
            .backend(backend)
            .model(&config.model)
            .max_tokens(config.max_tokens)
            .temperature(0.0)
            .schema(output_format)
            .system(system_prompt);

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

        let client = llm_builder.build().expect("Failed to build LLM");
        Ok(Self { client })
    }

    /// Invokes the LLM and cleans up the response.
    pub fn invoke(&self, request: &[ChatMessage]) -> Result<String, Box<dyn std::error::Error>> {
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("tokio failed")
            .block_on(self.client.chat(request))?
            .text()
            .expect("no response text");

        // Parse the response - strip markdown code fences
        let response = response.strip_prefix("```").unwrap_or(&response);
        let response = response.strip_prefix("json").unwrap_or(response);
        let response = response.strip_suffix("```").unwrap_or(response);

        Ok(response.to_string())
    }
}

/// Helper function to build a request from a prompt and serializable body.
///
/// Creates a request with three parts:
/// 1. The prompt string
/// 2. The body serialized to JSON
/// 3. "return as JSON" instruction
pub fn build_request<T: Serialize>(
    prompt: &str,
    body: &T,
) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error>> {
    let contents = [
        prompt.to_string(),
        serde_json::to_string(body)?,
        "return as JSON".to_string(),
    ];

    Ok(contents
        .iter()
        .map(|content| ChatMessage::user().content(content).build())
        .collect())
}
