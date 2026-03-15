//! `SplitAndFormat`: converts a `CargoPackage` into a `SplitPackage` by splitting its Rust
//! source into top-level declarations via `syn`, formatting each with `prettyplease`, and
//! pre-computing the `assembled_source` and `line_index` that the fix loop depends on.

use full_source::CargoPackage;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::fmt;
use std::fs;
use std::path::Path;
use tracing::{debug, warn};

/// A Cargo project decomposed into individually-addressable top-level declarations.
///
/// All fields are set once by `SplitAndFormat` (or by `FixDeclarationsLlm`) and are
/// read-only thereafter. The three derived fields (`assembled_source`, `line_index`) are
/// recomputed from `declarations` whenever a new `SplitPackage` is constructed.
pub struct SplitPackage {
    /// Each top-level declaration as a canonical string (formatted by `prettyplease`).
    /// Ordered exactly as in the original source file.
    pub declarations: Vec<String>,

    /// Full contents of `Cargo.toml`.
    pub cargo_toml: String,

    /// Relative path of the source file: `"src/main.rs"` or `"src/lib.rs"`.
    pub source_file_name: String,

    /// `declarations.join("\n\n")` stored here so that `FixBuildCheck` compiles exactly
    /// this string and `line_index` remains valid throughout the fix loop.
    pub assembled_source: String,

    /// `(start_line, end_line, decl_idx)`, 1-indexed.
    /// Covers every declaration; separator lines between declarations are not covered.
    pub line_index: Vec<(usize, usize, usize)>,
}

impl SplitPackage {
    /// Construct a `SplitPackage` from a list of declaration strings, computing
    /// `assembled_source` and `line_index` automatically.
    pub fn from_declarations(
        declarations: Vec<String>,
        cargo_toml: String,
        source_file_name: String,
    ) -> Self {
        let assembled_source = declarations
            .iter()
            .map(|d| d.trim_end_matches('\n'))
            .collect::<Vec<_>>()
            .join("\n\n");
        let line_index = build_line_index(&declarations);
        SplitPackage {
            declarations,
            cargo_toml,
            source_file_name,
            assembled_source,
            line_index,
        }
    }
}

/// Build the `(start_line, end_line, decl_idx)` index for `declarations`, 1-indexed.
fn build_line_index(declarations: &[String]) -> Vec<(usize, usize, usize)> {
    let mut result = Vec::with_capacity(declarations.len());
    let mut current_line: usize = 1;
    for (i, decl) in declarations.iter().enumerate() {
        let trimmed = decl.trim_end_matches('\n');
        let line_count = trimmed.lines().count().max(1);
        let start = current_line;
        let end = current_line + line_count - 1;
        result.push((start, end, i));
        // One blank separator line (`"\n\n"`) between consecutive declarations.
        current_line = end + 2;
    }
    result
}

impl fmt::Display for SplitPackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "SplitPackage ({} declarations, source: {}):",
            self.declarations.len(),
            self.source_file_name
        )?;
        write!(f, "{}", self.assembled_source)
    }
}

impl Representation for SplitPackage {
    fn name(&self) -> &'static str {
        "split_package"
    }

    /// Materializes the package to disk as a compilable Cargo project layout.
    /// Writes `Cargo.toml` and `src/<filename>` under `path`.
    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        fs::create_dir_all(path)?;
        fs::write(path.join("Cargo.toml"), &self.cargo_toml)?;
        // source_file_name is e.g. "src/main.rs"
        let src_path = path.join(&self.source_file_name);
        if let Some(parent) = src_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(src_path, &self.assembled_source)?;
        Ok(())
    }
}

// Splitter helpers

/// Format a single top-level `syn::Item` as a source string via `prettyplease`.
///
/// Falls back to the raw token-stream representation for `Item::Verbatim` because
/// `prettyplease` panics on those items.
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

/// Split `source` into top-level declaration strings, each formatted with `prettyplease`.
///
/// Falls back to a single "whole-file" declaration if `syn` cannot parse the source.
fn split_source(source: &str) -> Vec<String> {
    match syn::parse_file(source) {
        Ok(file) => file
            .items
            .iter()
            .map(|item| unparse_item(item).trim_end_matches('\n').to_string())
            .collect(),
        Err(e) => {
            warn!(
                "SplitAndFormat: syn failed to parse source, treating as single declaration: {e}"
            );
            vec![source.trim_end_matches('\n').to_string()]
        }
    }
}

// Tool

/// Converts a `CargoPackage` into a `SplitPackage` by splitting the source file into
/// top-level declarations and pre-computing the line index.
pub struct SplitAndFormat;

impl Tool for SplitAndFormat {
    fn name(&self) -> &'static str {
        "split_and_format"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("SplitAndFormat: no CargoPackage found in IR")?;

        let (source_file_name, source_bytes) =
            if let Ok(bytes) = cargo_package.dir.get_file("src/main.rs") {
                ("src/main.rs".to_string(), bytes.clone())
            } else if let Ok(bytes) = cargo_package.dir.get_file("src/lib.rs") {
                ("src/lib.rs".to_string(), bytes.clone())
            } else {
                return Err(
                    "SplitAndFormat: CargoPackage has neither src/main.rs nor src/lib.rs".into(),
                );
            };

        let cargo_toml_bytes = cargo_package
            .dir
            .get_file("Cargo.toml")
            .map_err(|_| "SplitAndFormat: CargoPackage missing Cargo.toml")?;
        let cargo_toml = String::from_utf8(cargo_toml_bytes.clone())?;

        let source = String::from_utf8(source_bytes)?;
        let declarations = split_source(&source);

        debug!(
            "SplitAndFormat: split {} into {} declarations",
            source_file_name,
            declarations.len()
        );

        Ok(Box::new(SplitPackage::from_declarations(
            declarations,
            cargo_toml,
            source_file_name,
        )))
    }
}
