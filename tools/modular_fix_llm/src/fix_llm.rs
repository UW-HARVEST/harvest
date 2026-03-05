//! LLM-based declaration fixer.
//!
//! Sends a broken declaration together with its compiler errors to an LLM
//! and receives the corrected source code.

use harvest_core::llm::{HarvestLLM, LLMConfig};
use serde::Deserialize;
use tracing::debug;

/// Structured output returned by the fix LLM.
#[derive(Debug, Deserialize)]
struct FixResult {
    fixed_code: String,
}

pub struct FixLlm {
    llm: HarvestLLM,
}

impl FixLlm {
    pub fn new(config: &LLMConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let system_prompt = include_str!("prompts/fix/system_prompt.txt");
        let schema = include_str!("prompts/fix/structured_schema.json");
        let llm = HarvestLLM::build(config, Some(schema), system_prompt)?;
        Ok(FixLlm { llm })
    }

    /// Ask the LLM to fix `decl_source` given the associated `errors_text` and `context`
    /// (the full interface context from the current repair state, with function bodies stubbed).
    /// Returns the corrected source for the declaration.
    pub fn fix_declaration(
        &self,
        decl_source: &str,
        errors_text: &str,
        context: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        debug!(
            "Sending declaration to fix LLM ({} bytes)",
            decl_source.len()
        );

        let prompt = include_str!("prompts/fix/user_prompt.txt")
            .replace("{context}", context)
            .replace("{errors}", errors_text)
            .replace("{declaration}", decl_source);

        let messages = vec![
            harvest_core::llm::ChatMessage::user()
                .content(&prompt)
                .build(),
        ];

        let response = self.llm.invoke(&messages)?;
        let result: FixResult = serde_json::from_str(&response).map_err(|e| {
            format!("Failed to parse fix LLM response as JSON: {e}\nResponse: {response}")
        })?;

        Ok(result.fixed_code.trim_end_matches('\n').to_string())
    }
}
