//! Types and utilities for working with top-level C declarations.

use c_ast::{ClangAst, TopLevelItem, TopLevelKind};

/// Container for categorizing declarations by their source.
#[derive(Debug, Clone)]
pub struct ClangNode<'a> {
    node: &'a TopLevelItem,
    pub visibility: Option<bool>,
}

impl<'a> ClangNode<'a> {
    pub fn new(node: &'a TopLevelItem) -> Self {
        Self {
            node,
            visibility: None,
        }
    }

    pub fn as_node(&self) -> &TopLevelItem {
        self.node
    }
}

#[derive(Debug)]
pub struct ClangDeclarations<'a> {
    /// Declarations imported from external sources (none in the current c_ast model).
    pub imported: Vec<ClangNode<'a>>,
    /// Type declarations from project source files.
    pub app_types: Vec<ClangNode<'a>>,
    /// Global variable declarations from project source files.
    pub app_globals: Vec<ClangNode<'a>>,
    /// Function declarations from project source files.
    pub app_functions: Vec<ClangNode<'a>>,
}

impl<'a> ClangDeclarations<'a> {
    /// Returns an iterator over function and global definitions.
    pub fn app_functions_and_globals(&self) -> impl Iterator<Item = ClangNode<'a>> + '_ {
        self.app_globals
            .iter()
            .cloned()
            .chain(self.app_functions.iter().cloned())
    }
}

/// Sorts and categorizes top-level items produced by `c_ast`.
pub fn extract_top_level_decls<'a>(clang_ast: &'a ClangAst) -> ClangDeclarations<'a> {
    let mut sorted_items: Vec<&TopLevelItem> = clang_ast.items.iter().collect();
    sorted_items.sort_by(|a, b| {
        a.span
            .file
            .cmp(&b.span.file)
            .then(a.span.start.offset.cmp(&b.span.start.offset))
            .then(a.span.end.offset.cmp(&b.span.end.offset))
    });

    let mut declarations = ClangDeclarations {
        imported: Vec::new(),
        app_types: Vec::new(),
        app_globals: Vec::new(),
        app_functions: Vec::new(),
    };

    for item in sorted_items {
        match item.kind {
            TopLevelKind::TypedefDecl | TopLevelKind::RecordDecl | TopLevelKind::EnumDecl => {
                declarations.app_types.push(ClangNode::new(item));
            }
            TopLevelKind::VarDecl => {
                declarations.app_globals.push(ClangNode::new(item));
            }
            TopLevelKind::FunctionDecl => {
                declarations.app_functions.push(ClangNode::new(item));
            }
            _ => {
                // Preprocessor-only items are intentionally ignored by translation passes.
            }
        }
    }

    declarations
}
