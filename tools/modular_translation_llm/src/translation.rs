//! Two-stage translation approach:
//! Stage A: TypedefDecl, RecordDecl, EnumDecl - decides data layout
//! Stage B: FunctionDecl, VarDecl - translates code operating over types (Vardecls included here because they make call initializers)
//!
//! Design decisions to come back to:
//! - Type results included as context for function/global translation
//! - No ordering constraints of function translations
//! - Cargo.toml generated after function/global translation using aggregated dependencies

use full_source::RawSource;
use identify_project_kind::ProjectKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tracing::{debug, info};

use crate::Config;
use crate::clang::ClangDeclarations;
use crate::translation_llm::ModularTranslationLLM;

/// Represents a translated Rust declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustDeclaration {
    /// The translated Rust code output
    pub rust_code: String,
    /// List of dependency module paths to import
    pub dependencies: Vec<String>,
}

/// Result of the type translation containing only type declarations
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

/// Translates type declarations to Rust using an LLM.
///
/// This function translates TypedefDecl, RecordDecl, and EnumDecl to establish
/// data layout for all types in the project. The results are then used as context
/// for function and global variable translation.
///
/// Returns only the translated type declarations (no Cargo.toml).
pub fn translate_types(
    type_decls: &[&clang_ast::Node<c_ast::Clang>],
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    modular_llm: &ModularTranslationLLM,
) -> Result<TypeTranslationResult, Box<dyn std::error::Error>> {
    debug!(
        "Starting type translation for {} declarations",
        type_decls.len()
    );

    if type_decls.is_empty() {
        // No types to translate, return empty result
        return Ok(TypeTranslationResult {
            translations: Vec::new(),
        });
    }

    let translation_result = modular_llm.translate_types(type_decls, raw_source, project_kind)?;

    if translation_result.translations.len() != type_decls.len() {
        return Err(format!(
            "Type translation: LLM returned {} translations but expected {}",
            translation_result.translations.len(),
            type_decls.len()
        )
        .into());
    }

    info!(
        "Type translation complete: successfully translated {} declarations",
        translation_result.translations.len()
    );

    Ok(translation_result)
}

/// Translates function and global variable declarations to Rust using an LLM.
///
/// This function translates FunctionDecl and VarDecl, with the type translations
/// provided as context. Each declaration is translated in its own request.
///
/// Returns the translated declarations.
pub fn translate_functions(
    function_and_global_decls: &[&clang_ast::Node<c_ast::Clang>],
    raw_source: &RawSource,
    project_kind: &ProjectKind,
    type_translations: &TypeTranslationResult,
    modular_llm: &ModularTranslationLLM,
) -> Result<Vec<RustDeclaration>, Box<dyn std::error::Error>> {
    debug!(
        "Starting function/global translation for {} declarations",
        function_and_global_decls.len()
    );

    if function_and_global_decls.is_empty() {
        return Err("No function or global declarations to translate".into());
    }

    let mut translations = Vec::new();

    for decl in function_and_global_decls {
        let translation = modular_llm.translate_function_global(
            decl,
            raw_source,
            project_kind,
            type_translations,
        )?;

        translations.push(translation);
    }

    info!(
        "Function/global translation complete: successfully translated {} declarations",
        translations.len()
    );

    Ok(translations)
}

fn collect_dependencies(translations: &[RustDeclaration]) -> Vec<String> {
    let mut dependency_set = BTreeSet::new();
    for translation in translations {
        for dependency in &translation.dependencies {
            dependency_set.insert(dependency.clone());
        }
    }
    dependency_set.into_iter().collect()
}

/// Orchestrates the translation of Clang declarations to Rust using an LLM.
///
/// First, translates type declarations (TypedefDecl, RecordDecl, EnumDecl)
/// Then, translates functions and globals (FunctionDecl, VarDecl) with type context
// Finally, generates a Cargo.toml manifest based on collected dependencies from all translations.
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
        "Starting translation of {} declarations ({} types, {} globals, {} functions)",
        total_decls,
        declarations.app_types.len(),
        declarations.app_globals.len(),
        declarations.app_functions.len()
    );

    if total_decls == 0 {
        return Err("No declarations to translate".into());
    }

    let modular_llm = ModularTranslationLLM::build(config)?;

    // Translate types
    let type_result = translate_types(
        &declarations.app_types,
        raw_source,
        project_kind,
        &modular_llm,
    )?;

    // Combine globals and functions for function/global translation
    let function_and_global_decls: Vec<_> = declarations.app_functions_and_globals().collect();

    // Translate functions and globals with type context
    let function_result = if function_and_global_decls.is_empty() {
        info!("No function or global declarations to translate");
        Vec::new()
    } else {
        translate_functions(
            &function_and_global_decls,
            raw_source,
            project_kind,
            &type_result,
            &modular_llm,
        )?
    };

    // Combine results: types first, then functions/globals
    let mut combined_translations = type_result.translations;
    combined_translations.extend(function_result);

    info!(
        "Translation complete: {} total declarations translated",
        combined_translations.len()
    );

    let dependencies = collect_dependencies(&combined_translations);
    let cargo_toml = modular_llm.generate_cargo_toml(dependencies, project_kind)?;

    Ok(TranslationResult {
        translations: combined_translations,
        cargo_toml,
    })
}
