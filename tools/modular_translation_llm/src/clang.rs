//! Types and utilities for working with Clang AST declarations.

use c_ast::{Clang, ClangAst};
use clang_ast::Node;
use full_source::RawSource;
use tracing::{debug, warn};

use crate::utils::get_file_from_location;

/// Returns true when the location originates from the original source (not from an include).
pub fn is_original(location: &clang_ast::SourceLocation) -> bool {
    let spelling_included_from = location
        .spelling_loc
        .as_ref()
        .and_then(|loc| loc.included_from.as_ref());
    let expansion_included_from = location
        .expansion_loc
        .as_ref()
        .and_then(|loc| loc.included_from.as_ref());

    spelling_included_from.is_none() && expansion_included_from.is_none()
}

/// Container for categorizing declarations by their source.
///
/// Three-pass translation approach:
/// - app_types: TypedefDecl, RecordDecl, EnumDecl (Pass 1 - data layout)
/// - app_functions + app_globals: FunctionDecl and VarDecl (Pass 2 - interface signatures)
/// - app_functions + app_globals: FunctionDecl and VarDecl (Pass 3 - implementations)
#[derive(Debug)]
pub struct ClangDeclarations<'a> {
    /// Declarations imported from external sources (not in the project source files)
    pub imported: Vec<&'a Node<Clang>>,
    /// Type declarations from the project source files (TypedefDecl) no RecordDecl, EnumDecl, as they are redundant with typedef
    pub app_types: Vec<&'a Node<Clang>>,
    /// Global variable declarations from the project source files (VarDecl)
    pub app_globals: Vec<&'a Node<Clang>>,
    /// Function declarations from the project source files (FunctionDecl)
    pub app_functions: Vec<&'a Node<Clang>>,
}

impl<'a> ClangDeclarations<'a> {
    /// Returns an iterator over function and global definitions (i.e., all the top-level definitions that we translate one-by-one).
    pub fn app_functions_and_globals(&'a self) -> impl Iterator<Item = &'a Node<Clang>> {
        self.app_globals
            .iter()
            .chain(self.app_functions.iter())
            .copied()
    }
}

/// Logs the declaration kind with appropriate log level.
/// The role of this function is to alert the user when we encounter an unexpected declaration kind
/// i.e., an AST node that should never be a top-level declaration.
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
/// This function assumes that the structure of the Clang AST is a list of TranslationUnitDecl nodes,
/// whose children are the top-level declarations.
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
        app_types: Vec::new(),
        app_globals: Vec::new(),
        app_functions: Vec::new(),
    };

    // Categorize declarations based on whether their file exists in source_files
    // and by their kind (types vs globals vs functions)
    for node in &top_level_nodes {
        for child in &node.inner {
            log_decl_kind(&child.kind);
            let loc = child.kind.loc();
            // Ensure this declaration is in our source code and not another imported library
            let is_source = get_file_from_location(&loc.cloned())
                .is_some_and(|file| source_files.dir.get_file(&file).is_ok());

            // Ensure we don't double include functions that are imported in several files (within our source)
            let is_original = loc.is_some_and(is_original);

            if is_source && is_original {
                // Categorize by declaration kind for two-pass translation
                match &child.kind {
                    Clang::TypedefDecl { .. } => {
                        declarations.app_types.push(child);
                    }
                    // | Clang::RecordDecl { .. }
                    // | Clang::EnumDecl { .. } => {
                    //     declarations.app_types.push(child);
                    // }
                    Clang::VarDecl { .. } => {
                        declarations.app_globals.push(child);
                    }
                    Clang::FunctionDecl { .. } => {
                        declarations.app_functions.push(child);
                    }
                    _ => {
                        // Other declaration types (like ParmVarDecl) are not expected at top level
                        // but won't cause failure - they're just not translated
                    }
                }
            } else {
                declarations.imported.push(child);
            }
        }
    }

    declarations
}
