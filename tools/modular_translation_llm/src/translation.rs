use full_source::RawSource;
use harvest_core::llm::{HarvestLLM, build_request};
use identify_project_kind::ProjectKind;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::Config;
use crate::clang::ClangDeclarations;
use crate::utils::read_source_at_range;
use harvest_core::llm::ChatMessage;

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

/// Result of the type translation (Pass 1) containing only type declarations
#[derive(Debug, Clone, Deserialize)]
pub struct TypeTranslationResult {
    pub translations: Vec<RustDeclaration>,
}

/// Result of the translation containing both declarations and Cargo.toml
#[derive(Debug, Deserialize)]
pub struct TranslationResult {
    pub translations: Vec<RustDeclaration>,
    pub cargo_toml: String,
}

/// Helper function to build a translation request for declarations.
fn build_decls_translation_request(
    declarations: &ClangDeclarations,
    raw_source: &RawSource,
    project_kind: &ProjectKind,
) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error>> {
    // Extract source text for all declarations (combining all three categories temporarily)
    let mut decl_sources = Vec::new();

    // Combine all declaration types in order: types, globals, functions
    let all_decls: Vec<_> = declarations
        .app_types
        .iter()
        .chain(declarations.app_globals.iter())
        .chain(declarations.app_functions.iter())
        .collect();

    for decl in all_decls {
        let source_text = if let Some(range) = decl.kind.range() {
            read_source_at_range(range, raw_source)?
        } else {
            return Err(format!("Declaration has no source range: {:?}", decl.kind).into());
        };
        decl_sources.push(DeclarationInput {
            source: source_text,
        });
    }

    #[derive(Serialize)]
    struct RequestWithContext {
        project_kind: String,
        declarations: Vec<DeclarationInput>,
    }

    let project_kind_str = match project_kind {
        ProjectKind::Executable => "executable",
        ProjectKind::Library => "library",
    };

    build_request(
        "Please translate the following C declarations to Rust:",
        &RequestWithContext {
            project_kind: project_kind_str.to_string(),
            declarations: decl_sources,
        },
    )
}

/// Translates multiple Clang declarations to Rust using an LLM.
///
/// This function batches all declarations and sends them to the LLM in a single request,
/// where each declaration is translated independently. It is batched in one request both to avoid
/// having to reason about API rate limits and to provide context across declarations in cases where all the
/// declarations fit in the context window.
///
/// Returns both the translated declarations and a generated Cargo.toml manifest.
pub fn translate_decls(
    declarations: &ClangDeclarations,
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    config: &Config,
) -> Result<TranslationResult, Box<dyn std::error::Error>> {
    let total_decls = declarations.app_types.len()
        + declarations.app_globals.len()
        + declarations.app_functions.len();

    debug!(
        "Starting translation of {} declarations ({} types, {} globals, {} functions)",
        total_decls,
        declarations.app_types.len(),
        declarations.app_globals.len(),
        declarations.app_functions.len()
    );

    if total_decls == 0 {
        return Err("No declarations to translate".into());
    }

    // Set up the LLM
    let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, SYSTEM_PROMPT)?;

    // Assemble the request
    let request = build_decls_translation_request(declarations, raw_source, project_kind)?;

    // Make the LLM call
    trace!("Sending request to LLM: {:?}", &request);
    let response = llm.invoke(&request)?;
    trace!("LLM responded: {:?}", &response);

    let translation_result: TranslationResult = serde_json::from_str(&response)?;

    if translation_result.translations.len() != total_decls {
        return Err(format!(
            "LLM returned {} translations but expected {}",
            translation_result.translations.len(),
            total_decls
        )
        .into());
    }

    debug!(
        "Successfully translated {} declarations",
        translation_result.translations.len()
    );

    Ok(translation_result)
}
