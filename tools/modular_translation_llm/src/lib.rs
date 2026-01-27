//! Modular translation for C->Rust. Decomposes a C project AST into its top-level modules and translates them one-by-one using an LLM.

use c_ast::{Clang, ClangAst};
use clang_ast::Node;
use full_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use identify_project_kind::ProjectKind;
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {}

/// Container for categorizing declarations by their source.
#[derive(Debug)]
pub struct Declarations<'a> {
    /// Declarations imported from external sources (not in the project source files)
    pub imported: Vec<&'a Node<Clang>>,
    /// Declarations from the project application files
    pub app: Vec<&'a Node<Clang>>,
}

impl<'a> Declarations<'a> {
    /// Logs all declarations with their source file and translation status.
    pub fn show_all(&self, source_files: &std::collections::HashSet<String>) {
        info!("Declarations:");
        info!("Total imported declarations: {}", self.imported.len());
        info!("Total app declarations: {}", self.app.len());
        for decl in self.imported.iter().chain(self.app.iter()) {
            let file = match &decl.kind {
                Clang::FunctionDecl { loc, .. }
                | Clang::TypedefDecl { loc, .. }
                | Clang::RecordDecl { loc, .. }
                | Clang::VarDecl { loc, .. } => get_file_from_location(loc),
                _ => "Unknown".to_string(),
            };
            let is_app = source_files.contains(&file);

            match &decl.kind {
                Clang::FunctionDecl { name, .. } => {
                    let scheduled = if is_app {
                        " - Scheduled to translate"
                    } else {
                        ""
                    };
                    info!("    FunctionDecl for {} from {}{}", name, file, scheduled);
                }
                Clang::TypedefDecl { name, .. } => {
                    let scheduled = if is_app {
                        " - Scheduled to translate"
                    } else {
                        ""
                    };
                    info!("    TypedefDecl for {} from {}{}", name, file, scheduled);
                }
                Clang::RecordDecl { name, .. } => {
                    let scheduled = if is_app {
                        " - Scheduled to translate"
                    } else {
                        ""
                    };
                    info!(
                        "    RecordDecl for {} from {}{}",
                        name.as_deref().unwrap_or("<anonymous>"),
                        file,
                        scheduled
                    );
                }
                Clang::VarDecl { name, .. } => {
                    let scheduled = if is_app {
                        " - Scheduled to translate"
                    } else {
                        ""
                    };
                    info!("    VarDecl for {} from {}{}", name, file, scheduled);
                }
                _ => {}
            }
        }
    }
}

/// Extracts all top-level translation unit declarations from a ClangAst and categorizes them.
///
/// This function iterates through all AST nodes in the ClangAst's HashMap,
/// asserts that each root node is of kind `TranslationUnitDecl`, then categorizes
/// all child declarations into imported (from external sources) and app (from project sources).
///
/// # Arguments
/// * `clang_ast` - The ClangAst structure containing parsed ASTs
/// * `source_files` - Set of file paths that are part of the project source
///
/// # Returns
/// A Declarations struct containing categorized declaration nodes
///
/// # Panics
/// Panics if any AST root node is not of kind `TranslationUnitDecl` or if an unexpected child kind is encountered
pub fn extract_top_level_decls<'a>(
    clang_ast: &'a ClangAst,
    source_files: &std::collections::HashSet<String>,
) -> Declarations<'a> {
    let top_level_nodes: Vec<&Node<Clang>> = clang_ast
        .asts
        .values()
        .map(|node| {
            // Assert that the node is a TranslationUnitDecl
            assert!(
                matches!(node.kind, Clang::TranslationUnitDecl),
                "Expected TranslationUnitDecl but found {:?}",
                node.kind
            );
            node
        })
        .collect();

    let mut declarations = Declarations {
        imported: Vec::new(),
        app: Vec::new(),
    };

    for node in &top_level_nodes {
        for child in &node.inner {
            let is_source = match &child.kind {
                Clang::FunctionDecl { loc, .. }
                | Clang::TypedefDecl { loc, .. }
                | Clang::RecordDecl { loc, .. }
                | Clang::VarDecl { loc, .. } => {
                    let file = get_file_from_location(loc);
                    source_files.contains(&file)
                }
                _ => {
                    panic!("Unexpected child kind: {:?}", child.kind);
                }
            };

            if is_source {
                declarations.app.push(child);
            } else {
                declarations.imported.push(child);
            }
        }
    }

    declarations
}

/// Extracts the file path from a SourceLocation.
///
/// # Arguments
/// * `loc` - An optional SourceLocation
///
/// # Returns
/// A string representing the file path, or "Unknown" if the location or file is not available
fn get_file_from_location(loc: &Option<clang_ast::SourceLocation>) -> String {
    loc.as_ref()
        .and_then(|l| l.spelling_loc.as_ref())
        .map(|sl| sl.file.to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

/// The main tool struct for modular translation.
pub struct ModularTranslationLlm;

/// Extracts and validates the tool's input arguments from the context.
///
/// # Arguments
/// * `context` - The run context containing the IR snapshot
/// * `inputs` - Vector of input IDs
///
/// # Returns
/// A tuple of (RawSource, ClangAst, ProjectKind) references
///
/// # Errors
/// Returns an error if any required representation is not found in the IR
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
        let (raw_source, clang_ast, _) = extract_args(&context, &inputs)?;

        // Get all source files from RawSource
        let source_files = raw_source.file_paths();

        // Extract and categorize top-level declarations
        let declarations = extract_top_level_decls(clang_ast, &source_files);
        declarations.show_all(&source_files);

        // Should return a CargoPackage representation
        Err("modular_translation_llm not yet implemented".into())
    }
}
