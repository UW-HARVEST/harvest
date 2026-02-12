//! LLM abstraction layer for modular translation.
//! Abstracts away all the string management needed for building dynamically generated prompts and
//! provides a clean well-typed interface for use by the rest of the transpiler.
use full_source::RawSource;
use harvest_core::llm::{HarvestLLM, build_request};
use identify_project_kind::ProjectKind;
use serde::Deserialize;
use serde::Serialize;

use crate::Config;
use crate::translation::{RustDeclaration, TypeTranslationResult};
use crate::utils::read_source_at_range;

/// Structured output JSON schema for Pass 1 (types).
const STRUCTURED_OUTPUT_SCHEMA_TYPES: &str =
    include_str!("prompts/type_translation/structured_schema.json");

/// Structured output JSON schema for Pass 2 (functions).
const STRUCTURED_OUTPUT_SCHEMA_FUNCTIONS: &str =
    include_str!("prompts/func_translation/structured_schema.json");

/// Structured output JSON schema for the function signature pass.
const STRUCTURED_OUTPUT_SCHEMA_FUNCTION_SIGNATURES: &str =
    include_str!("prompts/function_signatures/structured_schema.json");

/// Structured output JSON schema for Cargo.toml generation.
const STRUCTURED_OUTPUT_SCHEMA_CARGO_TOML: &str =
    include_str!("prompts/cargo_toml/structured_schema.json");

/// System prompt for Pass 1 (types).
const SYSTEM_PROMPT_TYPES: &str = include_str!("prompts/type_translation/system_prompt.txt");

/// System prompt for Pass 2 (functions).
const SYSTEM_PROMPT_FUNCTIONS: &str = include_str!("prompts/func_translation/system_prompt.txt");

/// System prompt for the function signature pass.
const SYSTEM_PROMPT_FUNCTION_SIGNATURES: &str =
    include_str!("prompts/function_signatures/system_prompt.txt");

/// System prompt for Cargo.toml generation.
const SYSTEM_PROMPT_CARGO_TOML: &str = include_str!("prompts/cargo_toml/system_prompt.txt");

/// Result of Cargo.toml generation.
#[derive(Debug, Deserialize)]
struct CargoTomlResult {
    pub cargo_toml: String,
}

/// Result of a single-pass function/global translation response.
#[derive(Debug, Deserialize)]
struct FunctionTranslationResult {
    pub translations: Vec<RustDeclaration>,
}

/// Result of function signature pass response.
#[derive(Debug, Deserialize)]
struct FunctionSignatureResult {
    pub signatures: Vec<String>,
}

/// LLM abstraction layer for modular translation.
/// Has support for 3 different types of LLM calls with different system prompts
// and structured output schemas:
/// - types_llm: for translating type declarations
/// - functions_llm: for translating function and global variable declarations one-by-one
/// - cargo_toml_llm: for generating Cargo.toml based on the list of dependencies used in the
//    translated code
pub struct ModularTranslationLLM {
    types_llm: HarvestLLM,
    signatures_llm: HarvestLLM,
    functions_llm: HarvestLLM,
    cargo_toml_llm: HarvestLLM,
}

impl ModularTranslationLLM {
    /// Initializes seperate HarvestLLM instances for each type of translation task with the
    // appropriate system prompts and structured output schemas.
    pub fn build(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let types_llm = HarvestLLM::build(
            &config.llm,
            STRUCTURED_OUTPUT_SCHEMA_TYPES,
            SYSTEM_PROMPT_TYPES,
        )?;
        let functions_llm = HarvestLLM::build(
            &config.llm,
            STRUCTURED_OUTPUT_SCHEMA_FUNCTIONS,
            SYSTEM_PROMPT_FUNCTIONS,
        )?;
        let signatures_llm = HarvestLLM::build(
            &config.llm,
            STRUCTURED_OUTPUT_SCHEMA_FUNCTION_SIGNATURES,
            SYSTEM_PROMPT_FUNCTION_SIGNATURES,
        )?;
        let cargo_toml_llm = HarvestLLM::build(
            &config.llm,
            STRUCTURED_OUTPUT_SCHEMA_CARGO_TOML,
            SYSTEM_PROMPT_CARGO_TOML,
        )?;

        Ok(Self {
            types_llm,
            signatures_llm,
            functions_llm,
            cargo_toml_llm,
        })
    }

    /// Translates type declarations to Rust using the types_llm.
    /// Arguments: - type_decls: list of Clang AST nodes corresponding to type declarations
    //               (TypedefDecl)
    ///            - raw_source: the full source code of the project.
    //               Used to retrieve the source text corresponding to each declaration.
    ///            - project_kind: the kind of project (executable or library) being translated.
    //               Used to decide whether we need to make these types #[repr(C)] (compatible with outside C code).
    pub fn translate_types(
        &self,
        type_decls: &[&clang_ast::Node<c_ast::Clang>],
        raw_source: &RawSource,
        project_kind: &ProjectKind,
    ) -> Result<TypeTranslationResult, Box<dyn std::error::Error>> {
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

        let request = build_request(
            "Please translate the following C type declarations to Rust:",
            &RequestWithContext {
                project_kind: project_kind_str.to_string(),
                declarations: decl_sources.clone(),
            },
        )?;

        let response = self.types_llm.invoke(&request)?;
        let translation_result: TypeTranslationResult = serde_json::from_str(&response)?;
        for (decl, translation) in decl_sources
            .iter()
            .zip(translation_result.translations.iter())
        {
            crate::info!(
                "Type Translation complete:\n {} \n==>\n {}",
                decl.source,
                translation.rust_code
            );
        }
        Ok(translation_result)
    }

    /// Translates a single function or global variable declaration to Rust using the
    //  functions_llm, with the type translations provided as context.
    /// Arguments: - decl: Clang AST node corresponding to either a FunctionDecl or VarDecl
    //               (global variable declaration)
    ///            - raw_source: the full source code of the project.
    //               Used to retrieve the source text corresponding to the declaration.
    ///            - project_kind: the kind of project (executable or library) being translated.
    //               Used to decide whether we need to make these declarations C-compatible.
    ///            - type_translations: the result of translating type declarations.
    //              Used as context for translating functions and globals.
    pub fn translate_function_global(
        &self,
        decl: &clang_ast::Node<c_ast::Clang>,
        raw_source: &RawSource,
        project_kind: &ProjectKind,
        type_translations: &TypeTranslationResult,
    ) -> Result<RustDeclaration, Box<dyn std::error::Error>> {
        let source_text = if let Some(range) = decl.kind.range() {
            read_source_at_range(range, raw_source)?
        } else {
            return Err(format!("Declaration has no source range: {:?}", decl.kind).into());
        };

        let decl_source = DeclarationInput {
            source: source_text,
        };

        #[derive(Serialize)]
        struct RequestWithContext {
            project_kind: String,
            type_translations: Vec<String>,
            declaration: DeclarationInput,
        }

        let project_kind_str = match project_kind {
            ProjectKind::Executable => "executable",
            ProjectKind::Library => "library",
        };

        let type_code: Vec<String> = type_translations
            .translations
            .iter()
            .map(|t| t.rust_code.clone())
            .collect();

        let request = build_request(
            "Please translate the following C function or global variable declaration to Rust. The type declarations have already been translated and are provided for context:",
            &RequestWithContext {
                project_kind: project_kind_str.to_string(),
                type_translations: type_code,
                declaration: decl_source.clone(),
            },
        )?;

        let response = self.functions_llm.invoke(&request)?;
        let translation_result: FunctionTranslationResult = serde_json::from_str(&response)?;

        if translation_result.translations.len() != 1 {
            return Err(format!(
                "Pass 2: LLM returned {} translations but expected 1",
                translation_result.translations.len()
            )
            .into());
        }
        let translations = translation_result.translations.into_iter().next().unwrap();
        crate::info!(
            "Function/Global Translation complete:\n {} \n==>\n {}",
            decl_source.source,
            translations.rust_code
        );
        Ok(translations)
    }

    /// Translates all function declarations to Rust signature lines using the signatures_llm.
    /// Arguments: - function_decls: list of Clang AST nodes corresponding to FunctionDecl
    ///            - raw_source: the full source code of the project.
    ///            - project_kind: the kind of project (executable or library) being translated.
    ///            - type_translations: the result of translating type declarations.
    ///              Used as context for translating signatures.
    pub fn translate_function_signatures(
        &self,
        function_decls: &[&clang_ast::Node<c_ast::Clang>],
        raw_source: &RawSource,
        project_kind: &ProjectKind,
        type_translations: &TypeTranslationResult,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mut decl_sources = Vec::new();

        for decl in function_decls {
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

        let type_code: Vec<String> = type_translations
            .translations
            .iter()
            .map(|t| t.rust_code.clone())
            .collect();

        let request = build_request(
            "Please translate the following C function declarations to Rust signature lines. The type declarations have already been translated and are provided for context:",
            &RequestWithContext {
                project_kind: project_kind_str.to_string(),
                type_translations: type_code,
                declarations: decl_sources.clone(),
            },
        )?;

        let response = self.signatures_llm.invoke(&request)?;
        let signature_result: FunctionSignatureResult = serde_json::from_str(&response)?;

        if signature_result.signatures.len() != decl_sources.len() {
            return Err(format!(
                "Signature pass: LLM returned {} signatures but expected {}",
                signature_result.signatures.len(),
                decl_sources.len()
            )
            .into());
        }

        for (decl, signature) in decl_sources.iter().zip(signature_result.signatures.iter()) {
            crate::info!(
                "Function Signature Translation complete:\n {} \n==>\n {}",
                decl.source,
                signature
            );
        }

        Ok(signature_result.signatures)
    }

    /// Generates a Cargo.toml manifest based on the list of dependencies used in the
    //  translated code, using the cargo_toml_llm.
    /// Arguments: - dependencies: list of dependency crate names used in the translated Rust code.
    //               Used as context for generating the Cargo.toml.
    ///            - project_kind: the kind of project (executable or library) being translated.
    //               Used to decide whether to generate a Cargo.toml for a binary or library project.
    pub fn generate_cargo_toml(
        &self,
        dependencies: Vec<String>,
        project_kind: &ProjectKind,
    ) -> Result<String, Box<dyn std::error::Error>> {
        #[derive(Serialize)]
        struct RequestWithContext {
            project_kind: String,
            dependencies: Vec<String>,
        }

        let project_kind_str = match project_kind {
            ProjectKind::Executable => "executable",
            ProjectKind::Library => "library",
        };

        let request = build_request(
            "Please generate a Cargo.toml manifest based on the project kind and dependency list:",
            &RequestWithContext {
                project_kind: project_kind_str.to_string(),
                dependencies: dependencies.clone(),
            },
        )?;

        let response = self.cargo_toml_llm.invoke(&request)?;
        let cargo_result: CargoTomlResult = serde_json::from_str(&response)?;
        crate::info!(
            "Cargo.toml Generation complete:\n {}:{:?} \n==>\n {}",
            project_kind_str,
            dependencies,
            cargo_result.cargo_toml
        );
        Ok(cargo_result.cargo_toml)
    }
}

#[derive(Debug, Serialize, Clone)]
struct DeclarationInput {
    source: String,
}
