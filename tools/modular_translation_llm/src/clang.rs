//! Types and utilities for working with Clang AST declarations.

use c_ast::{Clang, ClangAst};
use clang_ast::Node;
use full_source::RawSource;
use std::collections::HashSet;
use tracing::{debug, warn};

use crate::utils::get_file_from_location;

/// Container for categorizing declarations by their source.
///
/// Three-pass translation approach:
/// - app_types: TypedefDecl, RecordDecl, EnumDecl (Pass 1 - data layout)
/// - app_functions + app_globals: FunctionDecl and VarDecl (Pass 2 - interface signatures)
/// - app_functions + app_globals: FunctionDecl and VarDecl (Pass 3 - implementations)
#[derive(Debug, Clone)]
pub struct ClangNode<'a> {
    node: &'a Node<Clang>,
    pub visibility: Option<bool>,
}

impl<'a> ClangNode<'a> {
    pub fn new(node: &'a Node<Clang>) -> Self {
        Self {
            node,
            visibility: None,
        }
    }

    pub fn as_node(&self) -> &Node<Clang> {
        self.node
    }
}

#[derive(Debug)]
pub struct ClangDeclarations<'a> {
    /// Declarations imported from external sources (not in the project source files)
    pub imported: Vec<ClangNode<'a>>,
    /// Type declarations from the project source files (TypedefDecl) no RecordDecl, EnumDecl, as they are redundant with typedef
    pub app_types: Vec<ClangNode<'a>>,
    /// Global variable declarations from the project source files (VarDecl)
    pub app_globals: Vec<ClangNode<'a>>,
    /// Function declarations from the project source files (FunctionDecl)
    pub app_functions: Vec<ClangNode<'a>>,
}

impl<'a> ClangDeclarations<'a> {
    /// Returns an iterator over function and global definitions (i.e., all the top-level definitions that we translate one-by-one).
    pub fn app_functions_and_globals(&self) -> impl Iterator<Item = ClangNode<'a>> + '_ {
        self.app_globals
            .iter()
            .cloned()
            .chain(self.app_functions.iter().cloned())
    }

    /// Deduplicates declarations within each category (app_types, app_globals, app_functions).
    /// Declarations are considered duplicates if they have the same name.
    /// When duplicates are found, the one with spelling_loc.included_from == None is preferred.
    /// If multiple declarations meet this criteria, a warning is issued.
    pub fn deduplicate(&mut self) {
        deduplicate_decls(&mut self.app_types);
        deduplicate_decls(&mut self.app_globals);
        deduplicate_function_decls(&mut self.app_functions);
    }
}

/// Helper function to deduplicate a single category of declarations.
/// Deduplicates declarations with the same name, preferring those without an included_from field in their spelling location.
/// (If there is a seperate declaration in the header, this will prefer the implementation rather than the header).
/// TODO: technically, this function collapses the namespaces of structs and typedefs.
/// This should be ok, but worth checking.
fn deduplicate_decls(declarations: &mut Vec<ClangNode<'_>>) {
    use std::collections::HashMap;

    let mut seen_names: HashMap<String, (usize, bool)> = HashMap::new(); // (index, has_no_included_from)
    let mut to_remove = HashSet::new();

    for (idx, declaration) in declarations.iter().enumerate() {
        let node = declaration.as_node();
        if let Some(name) = node.kind.name() {
            let has_no_included_from = has_no_included_from_field(&node.kind);

            match seen_names.get(&name) {
                Some((existing_idx, existing_has_no_included_from)) => {
                    // We've seen this name before
                    if has_no_included_from && !existing_has_no_included_from {
                        // Current is better (has no included_from, existing does)
                        to_remove.insert(*existing_idx);
                        seen_names.insert(name, (idx, has_no_included_from));
                    } else if has_no_included_from && *existing_has_no_included_from {
                        // Both have no included_from - issue a warning
                        warn!(
                            "Multiple declarations with name '{}' have no included_from. \
                            Keeping the first one.",
                            name
                        );
                        to_remove.insert(idx);
                    } else {
                        // Current has included_from or existing is better
                        to_remove.insert(idx);
                    }
                }
                None => {
                    // First time seeing this name
                    seen_names.insert(name, (idx, has_no_included_from));
                }
            }
        }
    }

    // Remove duplicates in reverse order to maintain indices
    let mut indices_to_remove: Vec<usize> = to_remove.into_iter().collect();
    indices_to_remove.sort_by(|a, b| b.cmp(a)); // Reverse order
    for idx in indices_to_remove {
        declarations.remove(idx);
    }
}

/// Deduplicates function declarations with the same behavior as `deduplicate_decls`,
/// and additionally marks retained declarations as public when any declaration with
/// the same symbol name is found in a `.h` file.
fn deduplicate_function_decls(declarations: &mut Vec<ClangNode<'_>>) {
    use std::collections::HashMap;

    let mut seen_names: HashMap<String, (usize, bool)> = HashMap::new(); // (index, has_no_included_from)
    let mut to_remove = HashSet::new();
    let mut names_seen_in_header = HashSet::new();

    for (idx, declaration) in declarations.iter().enumerate() {
        let node = declaration.as_node();
        if let Some(name) = node.kind.name() {
            let has_no_included_from = has_no_included_from_field(&node.kind);

            if is_header_decl(node) {
                names_seen_in_header.insert(name.clone());
            }

            match seen_names.get(&name) {
                Some((existing_idx, existing_has_no_included_from)) => {
                    // We've seen this name before
                    if has_no_included_from && !existing_has_no_included_from {
                        // Current is better (has no included_from, existing does)
                        to_remove.insert(*existing_idx);
                        seen_names.insert(name, (idx, has_no_included_from));
                    } else if has_no_included_from && *existing_has_no_included_from {
                        // Both have no included_from - issue a warning
                        warn!(
                            "Multiple declarations with name '{}' have no included_from. \
                            Keeping the first one.",
                            name
                        );
                        to_remove.insert(idx);
                    } else {
                        // Current has included_from or existing is better
                        to_remove.insert(idx);
                    }
                }
                None => {
                    // First time seeing this name
                    seen_names.insert(name, (idx, has_no_included_from));
                }
            }
        }
    }

    // Remove duplicates in reverse order to maintain indices
    let mut indices_to_remove: Vec<usize> = to_remove.into_iter().collect();
    indices_to_remove.sort_by(|a, b| b.cmp(a)); // Reverse order
    for idx in indices_to_remove {
        declarations.remove(idx);
    }

    // Set visibility for deduplicated declarations: Some(true) when symbol appears in any header.
    for declaration in declarations.iter_mut() {
        if let Some(name) = declaration.as_node().kind.name()
            && names_seen_in_header.contains(&name)
        {
            declaration.visibility = Some(true);
        }
    }
}

fn is_header_decl(node: &Node<Clang>) -> bool {
    get_file_from_location(&node.kind.loc().cloned()).is_some_and(|file| file.ends_with(".h"))
}

/// Checks if a declaration's spelling location has no included_from field.
fn has_no_included_from_field(kind: &Clang) -> bool {
    kind.loc()
        .and_then(|loc| loc.spelling_loc.as_ref())
        .map(|bare_loc| bare_loc.included_from.is_none())
        .unwrap_or(false)
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
/// The key invariants are that the app_* declarations are all in our source code,
// and all correspond to unique spans.
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
            if is_source {
                // Categorize by declaration kind for two-pass translation
                match &child.kind {
                    Clang::TypedefDecl { .. }
                    | Clang::RecordDecl { .. }
                    | Clang::EnumDecl { .. } => {
                        declarations.app_types.push(ClangNode::new(child));
                    }
                    Clang::VarDecl { .. } => {
                        declarations.app_globals.push(ClangNode::new(child));
                    }
                    Clang::FunctionDecl { .. } => {
                        declarations.app_functions.push(ClangNode::new(child));
                    }
                    _ => {
                        // Other declaration types (like ParmVarDecl) are not expected at top level
                        // but won't cause failure - they're just not translated
                    }
                }
            } else {
                declarations.imported.push(ClangNode::new(child));
            }
        }
    }

    declarations.deduplicate();
    declarations
}
