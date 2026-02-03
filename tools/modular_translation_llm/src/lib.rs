//! Modular translation for C->Rust. Decomposes a C project AST into its top-level modules and translates them one-by-one using an LLM.

use c_ast::ClangAst;
use full_source::RawSource;
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::LLMConfig;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use identify_project_kind::ProjectKind;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use tracing::info;

mod clang;
mod recombine;
mod translation;
mod utils;
pub use clang::{ClangDeclarations, extract_top_level_decls};
pub use translation::{RustDeclaration, translate_decls};

/// Configuration for the modular translation tool.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub llm: LLMConfig,

    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.modular_translation_llm", &self.unknown);
    }

    /// Returns a mock config for testing.
    pub fn mock() -> Self {
        Self {
            llm: LLMConfig {
                address: None,
                api_key: None,
                backend: "mock_llm".into(),
                model: "mock_model".into(),
                max_tokens: 4000,
            },
            unknown: HashMap::new(),
        }
    }
}

/// The main tool struct for modular translation.
pub struct ModularTranslationLlm;

/// Extracts and validates the tool's input arguments from the context.
fn extract_args<'a>(
    context: &'a RunContext,
    inputs: &[Id],
) -> Result<(&'a RawSource, &'a ClangAst, &'a ProjectKind), Box<dyn std::error::Error>> {
    let raw_source = context
        .ir_snapshot
        .get::<RawSource>(inputs[0])
        .ok_or("No RawSource representation found in IR")?;
    let clang_ast = context
        .ir_snapshot
        .get::<ClangAst>(inputs[1])
        .ok_or("No ClangAst representation found in IR")?;
    let project_kind = context
        .ir_snapshot
        .get::<ProjectKind>(inputs[2])
        .ok_or("No ProjectKind representation found in IR")?;
    Ok((raw_source, clang_ast, project_kind))
}

impl Tool for ModularTranslationLlm {
    fn name(&self) -> &'static str {
        "modular_translation_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(
            context
                .config
                .tools
                .get("modular_translation_llm")
                .ok_or("No modular_translation_llm config found")?,
        )?;
        config.validate();

        let (raw_source, clang_ast, project_kind) = extract_args(&context, &inputs)?;

        // Extract and categorize top-level declarations
        let declarations = extract_top_level_decls(clang_ast, raw_source);

        // Translate all declarations
        let translations = translation::translate_decls(&declarations, raw_source, &config)?;

        info!(
            "Successfully translated {} declarations",
            translations.len()
        );

        // Assemble translations into a CargoPackage representation
        let package_name = "translated_project"; // TODO: Derive from source project
        let cargo_package = recombine::recombine_decls(translations, project_kind, package_name)?;

        Ok(Box::new(cargo_package))
    }
}
