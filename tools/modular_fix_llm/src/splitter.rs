//! Splits a Rust source file into top-level declarations using `syn`.
//!
//! Each declaration is re-serialised with `prettyplease` so that subsequent
//! LLM repairs receive clean, consistently-formatted source.

use tracing::warn;

/// A single top-level declaration extracted from a source file.
#[derive(Debug, Clone)]
pub struct ParsedDeclaration {
    /// Formatted source (from `prettyplease`), without a trailing newline.
    pub source: String,
}

/// Parse `source` into a list of top-level declarations.
///
/// Falls back to a single "whole file" declaration if `syn` cannot parse the
/// source (e.g. because the LLM output was not valid Rust).
pub fn split_source(source: &str) -> Vec<ParsedDeclaration> {
    match try_split(source) {
        Ok(decls) => decls,
        Err(e) => {
            warn!("syn failed to parse source, treating as single declaration: {e}");
            vec![ParsedDeclaration {
                source: source.trim_end_matches('\n').to_string(),
            }]
        }
    }
}

fn try_split(source: &str) -> Result<Vec<ParsedDeclaration>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut decls = Vec::with_capacity(file.items.len());

    for item in &file.items {
        // Re-serialise via prettyplease for clean formatting.
        let formatted = unparse_item(item);
        decls.push(ParsedDeclaration {
            source: formatted.trim_end_matches('\n').to_string(),
        });
    }

    Ok(decls)
}

/// Format a single top-level item as a source string.
///
/// `prettyplease` panics on `Item::Verbatim` (e.g. bare function prototypes ending with `;`
/// that `syn` cannot represent as a full `ItemFn`).  For those, we fall back to the raw
/// token-stream representation which is syntactically correct even if not pretty-printed.
pub fn unparse_item(item: &syn::Item) -> String {
    if let syn::Item::Verbatim(ts) = item {
        return ts.to_string();
    }
    let single_file = syn::File {
        shebang: None,
        attrs: vec![],
        items: vec![item.clone()],
    };
    prettyplease::unparse(&single_file)
}
