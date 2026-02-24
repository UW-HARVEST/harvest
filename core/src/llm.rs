//! This module provides a simplified synchronous wrapper around the `llm` crate
//! This simplifies Tools that relies on LLMs by providing a common configuration
//! and deduplicates common logic (like building requests and parsing responses).

use llm::LLMProvider;
use llm::builder::{LLMBackend, LLMBuilder};
pub use llm::chat::ChatMessage;
use llm::chat::StructuredOutputFormat;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, warn};

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

    /// Maximum number of retries on failure (default: 3).
    pub retry_count: Option<u32>,

    /// Seconds to wait between retries (default: 10).
    pub retry_delay_secs: Option<u64>,
}

/// Wrapper for an LLM client with helper methods.
pub struct HarvestLLM {
    client: Box<dyn LLMProvider>,
    retry_count: u32,
    retry_delay_secs: u64,
}

const DEFAULT_RETRY_COUNT: u32 = 4;
const DEFAULT_RETRY_DELAY_SECS: u64 = 31;

impl HarvestLLM {
    /// Builds an LLM client from configuration.
    ///
    /// # Arguments
    /// * `config` - LLM configuration (backend, model, etc.)
    /// * `output_format_json` - Optional JSON schema for structured output. Pass `None` for plain text output.
    /// * `system_prompt` - System prompt for the LLM
    pub fn build(
        config: &LLMConfig,
        output_format_json: Option<&str>,
        system_prompt: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let backend = LLMBackend::from_str(&config.backend).expect("unknown LLM_BACKEND");

        let mut llm_builder = LLMBuilder::new()
            .backend(backend)
            .model(&config.model)
            .max_tokens(config.max_tokens)
            .temperature(0.0)
            .system(system_prompt);

        // Only set schema if provided (for structured output)
        if let Some(schema_json) = output_format_json {
            let output_format: StructuredOutputFormat = serde_json::from_str(schema_json)?;
            llm_builder = llm_builder.schema(output_format);
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

        let client = llm_builder.build().expect("Failed to build LLM");
        Ok(Self {
            client,
            retry_count: config.retry_count.unwrap_or(DEFAULT_RETRY_COUNT),
            retry_delay_secs: config.retry_delay_secs.unwrap_or(DEFAULT_RETRY_DELAY_SECS),
        })
    }

    /// Invokes the LLM and cleans up the response.
    /// Retries up to `retry_count` times total (attempt 1 is the first try, not a retry).
    /// A 0-byte response is treated the same as a network/API error and triggers a retry.
    pub fn invoke(&self, request: &[ChatMessage]) -> Result<String, Box<dyn std::error::Error>> {
        let mut last_err = None;
        for attempt in 1..=self.retry_count {
            if attempt > 1 {
                warn!(
                    "Attempt {}/{} failed: {}. Waiting {}s...",
                    attempt - 1,
                    self.retry_count,
                    last_err.as_ref().unwrap(),
                    self.retry_delay_secs
                );
                std::thread::sleep(std::time::Duration::from_secs(self.retry_delay_secs));
                info!("Retrying (attempt {}/{})...", attempt, self.retry_count);
            }

            let result = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("tokio failed")
                .block_on(self.client.chat(request));

            match result {
                Ok(response) => {
                    let text = response.text().expect("no response text");

                    // Strip markdown code fences
                    let mut cleaned = text.as_str();
                    if let Some(rest) = cleaned.strip_prefix("```") {
                        cleaned = rest;
                        if let Some(rest) = cleaned.strip_prefix("rust") {
                            cleaned = rest;
                        } else if let Some(rest) = cleaned.strip_prefix("json") {
                            cleaned = rest;
                        }
                        cleaned = cleaned.trim_start_matches('\n').trim_start_matches('\r');
                    }
                    cleaned = cleaned.strip_suffix("```").unwrap_or(cleaned);
                    cleaned = cleaned.trim_end();

                    if cleaned.is_empty() {
                        last_err = Some(format!(
                            "attempt {}/{}: empty response (0 bytes)",
                            attempt, self.retry_count
                        ));
                        continue;
                    }

                    if attempt > 1 {
                        info!("Attempt {}/{} succeeded.", attempt, self.retry_count);
                    }
                    return Ok(cleaned.to_string());
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                }
            }
        }

        warn!(
            "Attempt {}/{} failed: {}",
            self.retry_count,
            self.retry_count,
            last_err.as_ref().unwrap()
        );
        Err(format!(
            "LLM call failed after {}/{} attempts: {}",
            self.retry_count,
            self.retry_count,
            last_err.unwrap()
        )
        .into())
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

/// Helper function to build a plain text request (without "return as JSON" instruction).
///
/// Use this for tools that expect plain text output instead of JSON.
pub fn build_plain_request<T: Serialize>(
    prompt: &str,
    body: &T,
) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error>> {
    let contents = [prompt.to_string(), serde_json::to_string(body)?];

    Ok(contents
        .iter()
        .map(|content| ChatMessage::user().content(content).build())
        .collect())
}
