//! Types and utilities for working with top-level C declarations.

use c_ast::TopLevelItem;

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
