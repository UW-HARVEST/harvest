//! Types and utilities for working with Clang AST declarations.

use c_ast::{Clang, ClangAst};
use clang_ast::Node;
use full_source::RawSource;
use tracing::{debug, warn};

use crate::utils::get_file_from_location;

/// Container for categorizing declarations by their source.
#[derive(Debug)]
pub struct ClangDeclarations<'a> {
    /// Declarations imported from external sources (not in the project source files)
    pub imported: Vec<&'a Node<Clang>>,
    /// Declarations from the project application files
    pub app: Vec<&'a Node<Clang>>,
}

/// Logs the declaration kind with appropriate log level.
fn log_decl_kind(kind: &c_ast::Clang) {
    match kind {
        c_ast::Clang::TranslationUnitDecl => {
            debug!("Processing TranslationUnitDecl");
        }
        c_ast::Clang::TypedefDecl { name, .. } => {
            debug!("Processing TypedefDecl: {}", name);
        }
        c_ast::Clang::FunctionDecl { name, .. } => {
            debug!("Processing FunctionDecl: {}", name);
        }
        c_ast::Clang::RecordDecl { name, .. } => {
            debug!("Processing RecordDecl: {:?}", name);
        }
        c_ast::Clang::VarDecl { name, .. } => {
            debug!("Processing VarDecl: {}", name);
        }
        c_ast::Clang::EnumDecl { name, .. } => {
            debug!("Processing EnumDecl: {:?}", name);
        }
        c_ast::Clang::ParmVarDecl { name, .. } => {
            warn!("Unexpected ParmVarDecl at top level: {:?}", name);
        }
        c_ast::Clang::CompoundStmt { .. } => {
            warn!("Unexpected CompoundStmt at top level");
        }
        c_ast::Clang::Other { kind, .. } => {
            warn!("Unexpected 'Other' declaration type: {:?}", kind);
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
/// A ClangDeclarations struct containing categorized declaration nodes
///
/// # Panics
/// Panics if any AST root node is not of kind `TranslationUnitDecl` or if an unexpected child kind is encountered
pub fn extract_top_level_decls<'a>(
    clang_ast: &'a ClangAst,
    source_files: &'a RawSource,
) -> ClangDeclarations<'a> {
    let top_level_nodes: Vec<&Node<Clang>> = clang_ast
        .asts
        .values()
        .inspect(|node| {
            // Assert that the node is a TranslationUnitDecl
            assert!(
                matches!(node.kind, Clang::TranslationUnitDecl),
                "Expected TranslationUnitDecl but found {:?}",
                node.kind
            );
        })
        .collect();

    let mut declarations = ClangDeclarations {
        imported: Vec::new(),
        app: Vec::new(),
    };

    // Categorize declarations based on whether their file exists in source_files
    for node in &top_level_nodes {
        for child in &node.inner {
            log_decl_kind(&child.kind);
            let is_source = child
                .kind
                .loc()
                .and_then(|loc| get_file_from_location(&Some(loc.clone())))
                .is_some_and(|file| source_files.dir.get_file(&file).is_ok());

            if is_source {
                declarations.app.push(child);
            } else {
                declarations.imported.push(child);
            }
        }
    }

    declarations
}
