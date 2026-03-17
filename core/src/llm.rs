//! This module provides a simplified synchronous wrapper around the `llm` crate
//! This simplifies Tools that relies on LLMs by providing a common configuration
//! and deduplicates common logic (like building requests and parsing responses).

use llm::LLMProvider;
use llm::builder::{LLMBackend, LLMBuilder};
use llm::chat::StructuredOutputFormat;
pub use llm::chat::{ChatMessage, Usage};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, warn};

/// Aggregated token usage across one or more LLM calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct LLMUsageTotals {
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl LLMUsageTotals {
    /// Adds a single call's usage to this aggregate. If usage is absent, no-op.
    pub fn add_usage(&mut self, usage: Option<&Usage>) {
        if let Some(usage) = usage {
            self.prompt_tokens += u64::from(usage.prompt_tokens);
            self.output_tokens += u64::from(usage.completion_tokens);
            self.total_tokens += u64::from(usage.total_tokens);
        }
    }
}

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

const DEFAULT_RETRY_COUNT: u32 = 3;
const DEFAULT_RETRY_DELAY_SECS: u64 = 10;

impl HarvestLLM {
    /// Builds an LLM client from configuration.
    ///
    /// # Arguments
    /// * `config` - LLM configuration (backend, model, etc.)
    /// * `output_format_json` - JSON schema for structured output.
    /// * `system_prompt` - System prompt for the LLM
    pub fn build(
        config: &LLMConfig,
        output_format_json: &str,
        system_prompt: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let backend = LLMBackend::from_str(&config.backend).expect("unknown LLM_BACKEND");

        // The llm crate's Bedrock backend supports tool use but has an incomplete
        // hardcoded model capability list. Override it to enable tool use for all
        // models, since HARVEST requires structured output via tool use.
        if backend == LLMBackend::AwsBedrock {
            // SAFETY: Although this runs within a tool thread, it is safe because
            // only one translation tool executes per run, and all HarvestLLM::build()
            // calls within that tool use the same model config, making writes idempotent.
            unsafe {
                std::env::set_var(
                    "LLM_BEDROCK_MODEL_CAPABILITIES",
                    format!(
                        r#"{{"models":{{"{}":{{"tool_use":true,"chat":true}}}}}}"#,
                        config.model
                    ),
                );
            }
        }

        let mut llm_builder = LLMBuilder::new()
            .backend(backend)
            .model(&config.model)
            .max_tokens(config.max_tokens)
            .temperature(0.0);

        let output_format: StructuredOutputFormat = serde_json::from_str(output_format_json)?;
        llm_builder = llm_builder.schema(output_format).system(system_prompt);

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

    /// Invokes the LLM with the given messages once.
    ///
    /// Helper for [Self::invoke]
    fn invoke_once(
        &self,
        request: &[ChatMessage],
    ) -> Result<(String, Option<Usage>), Box<dyn std::error::Error>> {
        let response = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("tokio failed")
            .block_on(self.client.chat(request))?;

        let usage = response.usage();
        let response_text = response.text().expect("no response text");

        // Parse the response - strip markdown code fences
        let response_text = response_text
            .strip_prefix("```json")
            .or_else(|| response_text.strip_prefix("```rust"))
            .or_else(|| response_text.strip_prefix("```"))
            .and_then(|t| t.strip_suffix("```"))
            .unwrap_or(&response_text)
            .trim();

        if response_text.is_empty() {
            Err("empty response (0 bytes)".into())
        } else {
            Ok((response_text.to_string(), usage))
        }
    }

    /// Invoke the LLM with the provided messages and clean up the
    /// reponse
    ///
    /// Retries up to [Self::retry_count] times total.  A 0-byte
    /// response is treated the same as an error and triggers a retry.
    pub fn invoke(
        &self,
        request: &[ChatMessage],
    ) -> Result<(String, Option<Usage>), Box<dyn std::error::Error>> {
        let mut attempt = 0;
        let last_err = loop {
            if attempt > 0 {
                info!("Retrying (attempt {}/{})...", attempt, self.retry_count);
            }
            match (attempt, self.invoke_once(request)) {
                (_, resp @ Ok(_)) => return resp,
                (a, Err(e)) if a >= self.retry_count => {
                    break e;
                }
                (a, Err(e)) => {
                    warn!(
                        "Attempt {}/{} failed: {}. Waiting {}s...",
                        a, self.retry_count, e, self.retry_delay_secs
                    );
                    std::thread::sleep(std::time::Duration::from_secs(self.retry_delay_secs));
                }
            }
            attempt += 1;
        };

        warn!(
            "Attempt {}/{} failed: {}",
            self.retry_count, self.retry_count, last_err
        );
        Err(format!(
            "LLM call failed after {}/{} attempts: {}",
            self.retry_count, self.retry_count, last_err
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
