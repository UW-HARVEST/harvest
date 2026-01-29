//! Types and utilities for working with Clang AST declarations.

use c_ast::{Clang, ClangAst};
use clang_ast::Node;
use tracing::info;

use crate::utils::get_file_from_location;

/// Container for categorizing declarations by their source.
#[derive(Debug)]
pub struct ClangDeclarations<'a> {
    /// Declarations imported from external sources (not in the project source files)
    pub imported: Vec<&'a Node<Clang>>,
    /// Declarations from the project application files
    pub app: Vec<&'a Node<Clang>>,
}

impl<'a> ClangDeclarations<'a> {
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
/// A ClangDeclarations struct containing categorized declaration nodes
///
/// # Panics
/// Panics if any AST root node is not of kind `TranslationUnitDecl` or if an unexpected child kind is encountered
pub fn extract_top_level_decls<'a>(
    clang_ast: &'a ClangAst,
    source_files: &std::collections::HashSet<String>,
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

    for node in &top_level_nodes {
        for child in &node.inner {
            let is_source = if let Some(loc) = child.kind.loc() {
                let file = get_file_from_location(&Some(loc.clone()));
                source_files.contains(&file)
            } else {
                panic!("Unexpected child kind without location: {:?}", child.kind);
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
