use std::collections::HashMap;

use harvest_core::config::unknown_field_warning;
use harvest_core::llm::{ChatMessage, HarvestLLM, LLMConfig};
use serde::Deserialize;
use serde_json::Value;

/// Configuration read from `[tools.fix_declarations_llm]`.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub llm: LLMConfig,

    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.fix_declarations_llm", &self.unknown);
    }
}

// LLM wrapper

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
        let llm = HarvestLLM::build(config, schema, system_prompt)?;
        Ok(FixLlm { llm })
    }

    pub fn fix_declaration(
        &self,
        decl_source: &str,
        errors_text: &str,
        context: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let prompt = include_str!("prompts/fix/user_prompt.txt")
            .replace("{context}", context)
            .replace("{errors}", errors_text)
            .replace("{declaration}", decl_source);

        let messages = vec![ChatMessage::user().content(&prompt).build()];
        let (response, _usage) = self.llm.invoke(&messages)?;
        let result: FixResult = serde_json::from_str(&response).map_err(|e| {
            format!("Failed to parse fix LLM response as JSON: {e}\nResponse: {response}")
        })?;

        Ok(result.fixed_code.trim_end_matches('\n').to_string())
    }
}
