//! Two-pass translation approach:
//! Pass 1: TypedefDecl, RecordDecl, EnumDecl - decides data layout
//! Pass 2: FunctionDecl, VarDecl - translates code operating over types (Vardecls included here because they make call initializers)
//!
//! Design decisions to come back to:
//! - Pass 1 results included as context for pass 2
//! - No ordering constraints of function translations
//! - Cargo.toml generated only in pass 2 (its not clear how well this works yet)

use full_source::RawSource;
use harvest_core::llm::{HarvestLLM, build_request};
use identify_project_kind::ProjectKind;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, trace};

use crate::Config;
use crate::clang::ClangDeclarations;
use crate::utils::read_source_at_range;
use harvest_core::llm::ChatMessage;

/// Structured output JSON schema for Pass 1 (types).
const STRUCTURED_OUTPUT_SCHEMA_TYPES: &str =
    include_str!("prompts/type_translation/structured_schema.json");

/// Structured output JSON schema for Pass 2 (functions).
const STRUCTURED_OUTPUT_SCHEMA_FUNCTIONS: &str =
    include_str!("prompts/func_translation/structured_schema.json");

/// System prompt for Pass 1 (types).
const SYSTEM_PROMPT_TYPES: &str = include_str!("prompts/type_translation/system_prompt.txt");

/// System prompt for Pass 2 (functions).
const SYSTEM_PROMPT_FUNCTIONS: &str = include_str!("prompts/func_translation/system_prompt.txt");

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeTranslationResult {
    pub translations: Vec<RustDeclaration>,
}

/// Result of the translation containing both declarations and Cargo.toml
#[derive(Debug, Deserialize)]
pub struct TranslationResult {
    pub translations: Vec<RustDeclaration>,
    pub cargo_toml: String,
}

/// Helper function to build a translation request for type declarations (Pass 1).
fn build_types_translation_request(
    type_decls: &[&clang_ast::Node<c_ast::Clang>],
    raw_source: &RawSource,
    project_kind: &ProjectKind,
) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error>> {
    let mut decl_sources = Vec::new();

    for decl in type_decls {
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
        "Please translate the following C type declarations to Rust:",
        &RequestWithContext {
            project_kind: project_kind_str.to_string(),
            declarations: decl_sources,
        },
    )
}

/// Helper function to build a translation request for function/global declarations (Pass 2).
fn build_functions_translation_request(
    function_and_global_decls: &[&clang_ast::Node<c_ast::Clang>],
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    type_translations: &TypeTranslationResult,
) -> Result<Vec<ChatMessage>, Box<dyn std::error::Error>> {
    let mut decl_sources = Vec::new();

    for decl in function_and_global_decls {
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
        type_translations: Vec<String>,
        declarations: Vec<DeclarationInput>,
    }

    let project_kind_str = match project_kind {
        ProjectKind::Executable => "executable",
        ProjectKind::Library => "library",
    };

    // Include the type translations as context
    let type_code: Vec<String> = type_translations
        .translations
        .iter()
        .map(|t| t.rust_code.clone())
        .collect();

    build_request(
        "Please translate the following C function and global variable declarations to Rust. The type declarations have already been translated and are provided for context:",
        &RequestWithContext {
            project_kind: project_kind_str.to_string(),
            type_translations: type_code,
            declarations: decl_sources,
        },
    )
}

/// Translates type declarations (Pass 1) to Rust using an LLM.
///
/// This function translates TypedefDecl, RecordDecl, and EnumDecl to establish
/// data layout for all types in the project. The results are then used as context
/// for Pass 2 (function and global variable translation).
///
/// Returns only the translated type declarations (no Cargo.toml).
pub fn translate_types(
    type_decls: &[&clang_ast::Node<c_ast::Clang>],
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    config: &Config,
) -> Result<TypeTranslationResult, Box<dyn std::error::Error>> {
    debug!(
        "Starting Pass 1: translating {} type declarations",
        type_decls.len()
    );

    if type_decls.is_empty() {
        // No types to translate, return empty result
        return Ok(TypeTranslationResult {
            translations: Vec::new(),
        });
    }

    // Set up the LLM for type translation
    let llm = HarvestLLM::build(
        &config.llm,
        STRUCTURED_OUTPUT_SCHEMA_TYPES,
        SYSTEM_PROMPT_TYPES,
    )?;

    // Assemble the request
    let request = build_types_translation_request(type_decls, raw_source, project_kind)?;

    // Make the LLM call
    trace!("Sending Pass 1 request to LLM: {:?}", &request);
    let response = llm.invoke(&request)?;
    trace!("Pass 1 LLM responded: {:?}", &response);

    let translation_result: TypeTranslationResult = serde_json::from_str(&response)?;

    if translation_result.translations.len() != type_decls.len() {
        return Err(format!(
            "Pass 1: LLM returned {} translations but expected {}",
            translation_result.translations.len(),
            type_decls.len()
        )
        .into());
    }

    info!(
        "Pass 1 complete: successfully translated {} type declarations",
        translation_result.translations.len()
    );

    Ok(translation_result)
}

/// Translates function and global variable declarations (Pass 2) to Rust using an LLM.
///
/// This function translates FunctionDecl and VarDecl, with the type translations from
/// Pass 1 provided as context. This pass also generates the Cargo.toml manifest.
///
/// Returns the translated declarations and Cargo.toml.
pub fn translate_functions(
    function_and_global_decls: &[&clang_ast::Node<c_ast::Clang>],
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    type_translations: &TypeTranslationResult,
    config: &Config,
) -> Result<TranslationResult, Box<dyn std::error::Error>> {
    debug!(
        "Starting Pass 2: translating {} function/global declarations",
        function_and_global_decls.len()
    );

    if function_and_global_decls.is_empty() {
        return Err("Pass 2: No function or global declarations to translate".into());
    }

    // Set up the LLM for function translation
    let llm = HarvestLLM::build(
        &config.llm,
        STRUCTURED_OUTPUT_SCHEMA_FUNCTIONS,
        SYSTEM_PROMPT_FUNCTIONS,
    )?;

    // Assemble the request with type context
    let request = build_functions_translation_request(
        function_and_global_decls,
        raw_source,
        project_kind,
        type_translations,
    )?;

    // Make the LLM call
    trace!("Sending Pass 2 request to LLM: {:?}", &request);
    let response = llm.invoke(&request)?;
    trace!("Pass 2 LLM responded: {:?}", &response);

    let translation_result: TranslationResult = serde_json::from_str(&response)?;

    if translation_result.translations.len() != function_and_global_decls.len() {
        return Err(format!(
            "Pass 2: LLM returned {} translations but expected {}",
            translation_result.translations.len(),
            function_and_global_decls.len()
        )
        .into());
    }

    info!(
        "Pass 2 complete: successfully translated {} function/global declarations",
        translation_result.translations.len()
    );

    Ok(translation_result)
}

/// Orchestrates the two-pass translation of Clang declarations to Rust using an LLM.
///
/// Pass 1: Translates type declarations (TypedefDecl, RecordDecl, EnumDecl)
/// Pass 2: Translates functions and globals (FunctionDecl, VarDecl) with type context
///
/// Returns the combined translated declarations and a generated Cargo.toml manifest.
pub fn translate_decls(
    declarations: &ClangDeclarations,
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    config: &Config,
) -> Result<TranslationResult, Box<dyn std::error::Error>> {
    let total_decls = declarations.app_types.len()
        + declarations.app_globals.len()
        + declarations.app_functions.len();

    info!(
        "Starting two-pass translation of {} declarations ({} types, {} globals, {} functions)",
        total_decls,
        declarations.app_types.len(),
        declarations.app_globals.len(),
        declarations.app_functions.len()
    );

    if total_decls == 0 {
        return Err("No declarations to translate".into());
    }

    // Pass 1: Translate types
    let type_result = translate_types(&declarations.app_types, raw_source, project_kind, config)?;

    // Combine globals and functions for Pass 2
    let function_and_global_decls: Vec<_> = declarations
        .app_globals
        .iter()
        .chain(declarations.app_functions.iter())
        .copied()
        .collect();

    if function_and_global_decls.is_empty() {
        return Err("No function or global declarations to translate in Pass 2".into());
    }

    // Pass 2: Translate functions and globals with type context
    let mut function_result = translate_functions(
        &function_and_global_decls,
        raw_source,
        project_kind,
        &type_result,
        config,
    )?;

    // Combine results: types first, then functions/globals
    let mut combined_translations = type_result.translations;
    combined_translations.append(&mut function_result.translations);

    info!(
        "Two-pass translation complete: {} total declarations translated",
        combined_translations.len()
    );

    Ok(TranslationResult {
        translations: combined_translations,
        cargo_toml: function_result.cargo_toml,
    })
}
