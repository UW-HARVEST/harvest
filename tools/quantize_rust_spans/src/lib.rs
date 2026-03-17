//! A [Tool] and [Representation] to deconstruct a [CargoPackage] into
//! the top-level items in each Rust source file.

use full_source::CargoPackage;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fmt, fs};
use syn::spanned::Spanned;
use tracing::{debug, warn};

// Copy from proc_macro2 so we can derive [Serialize] and [Deserialize]
/// A line-column pair representing the start or end of a `Span`.
///
/// This type is semver exempt and not exposed by default.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct LineColumn {
    /// The 1-indexed line in the source file on which the span starts or ends
    /// (inclusive).
    pub line: usize,
    /// The 0-indexed column (in UTF-8 characters) in the source file on which
    /// the span starts or ends (inclusive).
    pub column: usize,
}

impl From<proc_macro2::LineColumn> for LineColumn {
    fn from(value: proc_macro2::LineColumn) -> Self {
        Self {
            line: value.line,
            column: value.column,
        }
    }
}

impl Ord for LineColumn {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.line
            .cmp(&other.line)
            .then(self.column.cmp(&other.column))
    }
}

impl PartialOrd for LineColumn {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A [Representation] that maps top-level items in a [CargoPackage].
///
/// Stores a list of spans of top-level items in each Rust file
/// of a [CargoPackage].
pub struct RustItemMap {
    /// [Representation] index of the [CargoPackage] from which this
    /// [Representation] is derived.
    pub cargo_pkg_idx: Id,

    /// Stores a list of spans (starting [LineColumn] and ending
    /// [LineColumn]) for top-level items in each Rust file in
    /// a [CargoPackage]. Keys are full paths relative to the root of
    /// the [CargoPackage].
    pub items: HashMap<PathBuf, Vec<(LineColumn, LineColumn)>>,
}

impl fmt::Display for RustItemMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "RustItemMap ({} files):", self.items.len())?;
        for (path, items) in self.items.iter() {
            writeln!(f, "\t{}: {} items", path.display(), items.len())?;
        }
        Ok(())
    }
}

impl Representation for RustItemMap {
    fn name(&self) -> &'static str {
        "rust_items_map"
    }

    /// Materializes the package to disk as a compilable Cargo project layout.
    /// Writes `Cargo.toml` and `src/<filename>` under `path`.
    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        fs::create_dir_all(path)?;
        fs::write(
            path.join("_cargo_package_idx"),
            serde_json::ser::to_vec(&self.cargo_pkg_idx)?,
        )?;
        for (path, items) in self.items.iter() {
            // source_file_name is e.g. "src/main.rs"
            let full_path = path.join(path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(full_path, serde_json::ser::to_vec(items)?)?;
        }
        Ok(())
    }
}

/// Split `source` into top-level items strings, each formatted with `prettyplease`.
///
/// Falls back to a single "whole-file" items if `syn` cannot parse the source.
fn extract_top_level_spans(
    source: &str,
) -> Result<Vec<(LineColumn, LineColumn)>, impl std::error::Error> {
    syn::parse_file(source).map(|file| {
        file.items
            .iter()
            .map(|item| {
                let span = item.span();
                (span.start().into(), span.end().into())
            })
            .collect()
    })
}

// Tool

/// A [Tool] to deconstruct a [CargoPackage] into the top-level items
/// in each Rust source file.
pub struct QuantizeRustSpans;

impl Tool for QuantizeRustSpans {
    fn name(&self) -> &'static str {
        "quantize_rust_spans"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let cargo_pkg_idx = inputs[0];
        let cargo_pkg = context
            .ir_snapshot
            .get::<CargoPackage>(cargo_pkg_idx)
            .ok_or("QuantizeRustSpans: no CargoPackage found in IR")?;

        let mut items: HashMap<PathBuf, Vec<(LineColumn, LineColumn)>> = HashMap::new();

        let source_files = cargo_pkg
            .dir
            .files_recursive()
            .into_iter()
            .filter(|(path, _)| path.ends_with(".rs"));
        for (path, source) in source_files {
            let source = str::from_utf8(source)?;
            match extract_top_level_spans(source) {
                Ok(decls) => {
                    debug!(
                        "QuantizeRustSpans: split {} into {} items",
                        path.display(),
                        decls.len()
                    );

                    items.insert(path, decls);
                }
                Err(e) => {
                    warn!("syn failed to parse source, treating as single items: {e}");
                    items.insert(path, vec![]);
                }
            }
        }

        Ok(Box::new(RustItemMap {
            cargo_pkg_idx,
            items,
        }))
    }
}
