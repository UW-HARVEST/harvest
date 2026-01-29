//! Utilities for translating C declarations to Rust.

use full_source::RawSource;
use harvest_core::llm::{HarvestLLM, build_request};
use llm::chat::{ChatMessage, StructuredOutputFormat};
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::Config;
use crate::clang::ClangDeclarations;
use crate::utils::read_source_at_range;

/// Structured output JSON schema for LLM.
const STRUCTURED_OUTPUT_SCHEMA: &str = include_str!("structured_schema.json");

/// System prompt for translation.
const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

/// Represents a translated Rust declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustDeclaration {
    /// The translated Rust code output
    pub rust_code: String,
    /// List of dependency module paths to import
    pub dependencies: Vec<String>,
}

/// Internal structure for batching declarations to the LLM.
#[derive(Debug, Serialize)]
struct DeclarationInput {
    source: String,
}

/// Internal structure for the batch request.
#[derive(Debug, Serialize)]
struct DeclarationsRequest {
    declarations: Vec<DeclarationInput>,
}

/// Internal structure for the batch response.
#[derive(Debug, Deserialize)]
struct TranslationsResponse {
    translations: Vec<RustDeclaration>,
}

/// Helper function to build a translation request for declarations.
fn build_decls_translation_request(
    declarations: &ClangDeclarations,
    raw_source: &RawSource,
) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error>> {
    // Extract source text for all declarations
    let mut decl_sources = Vec::new();
    for decl in &declarations.app {
        let source_text = if let Some(range) = decl.kind.range() {
            read_source_at_range(range, raw_source)?
        } else {
            return Err(format!("Declaration has no source range: {:?}", decl.kind).into());
        };
        decl_sources.push(DeclarationInput {
            source: source_text,
        });
    }

    let request_body = DeclarationsRequest {
        declarations: decl_sources,
    };

    build_request(
        "Please translate the following C code to Rust:",
        &request_body,
    )
}

/// Translates multiple Clang declarations to Rust using an LLM.
///
/// This function batches all declarations and sends them to the LLM in a single request,
/// where each declaration is translated independently.
pub fn translate_decls(
    declarations: &ClangDeclarations,
    raw_source: &RawSource,
    config: &Config,
) -> Result<Vec<RustDeclaration>, Box<dyn std::error::Error>> {
    debug!(
        "Starting translation of {} declarations",
        declarations.app.len()
    );

    if declarations.app.is_empty() {
        debug!("No declarations to translate");
        return Ok(Vec::new());
    }

    // Set up the LLM
    let output_format: StructuredOutputFormat = serde_json::from_str(STRUCTURED_OUTPUT_SCHEMA)?;
    let llm = HarvestLLM::build(&config.llm, output_format, SYSTEM_PROMPT)?;

    // Assemble the request
    let request = build_decls_translation_request(declarations, raw_source)?;

    // Make the LLM call
    trace!(
        "Making LLM call with {} declarations",
        declarations.app.len()
    );
    let response = llm.invoke(&request)?;
    trace!("LLM responded: {:?}", &response);

    let translations: TranslationsResponse = serde_json::from_str(&response)?;

    if translations.translations.len() != declarations.app.len() {
        return Err(format!(
            "LLM returned {} translations but expected {}",
            translations.translations.len(),
            declarations.app.len()
        )
        .into());
    }

    debug!(
        "Successfully translated {} declarations",
        translations.translations.len()
    );
    Ok(translations.translations)
}
