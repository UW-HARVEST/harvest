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
        let single_file = syn::File {
            shebang: None,
            attrs: vec![],
            items: vec![item.clone()],
        };
        let formatted = prettyplease::unparse(&single_file);
        decls.push(ParsedDeclaration {
            source: formatted.trim_end_matches('\n').to_string(),
        });
    }

    Ok(decls)
}
