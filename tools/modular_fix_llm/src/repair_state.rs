//! Internal mutable state for the iterative repair loop.
//!
//! `RepairState` holds the current set of declarations and the Cargo.toml,
//! and knows how to assemble them into a `CargoPackage` for each build attempt.

use crate::splitter::{ParsedDeclaration, split_source};
use full_source::CargoPackage;
use harvest_core::fs::RawDir;
use tracing::debug;

/// One mutable declaration tracked during the repair loop.
#[derive(Debug, Clone)]
pub struct MutableDeclaration {
    /// Current Rust source for this declaration (may have been patched by the LLM).
    pub source: String,
}

/// All mutable state needed to drive the repair loop.
pub struct RepairState {
    pub declarations: Vec<MutableDeclaration>,
    pub cargo_toml: String,
    /// `"src/main.rs"` or `"src/lib.rs"`.
    pub source_file_name: String,
    /// Cached line index: `(line_start, line_end, decl_idx)`, 1-indexed.
    /// `None` means the index is stale and must be rebuilt before use.
    line_index: Option<Vec<(usize, usize, usize)>>,
}

impl RepairState {
    /// Build a `RepairState` from an existing `CargoPackage`.
    ///
    /// The `source_file_name` is inferred from the package's directory contents.
    pub fn from_cargo_package(pkg: &CargoPackage) -> Result<Self, Box<dyn std::error::Error>> {
        let (source_file_name, source_bytes) = if let Ok(bytes) = pkg.dir.get_file("src/main.rs") {
            ("src/main.rs".to_string(), bytes.clone())
        } else if let Ok(bytes) = pkg.dir.get_file("src/lib.rs") {
            ("src/lib.rs".to_string(), bytes.clone())
        } else {
            return Err("CargoPackage contains neither src/main.rs nor src/lib.rs".into());
        };

        let cargo_toml_bytes = pkg
            .dir
            .get_file("Cargo.toml")
            .map_err(|_| "CargoPackage missing Cargo.toml")?;
        let cargo_toml = String::from_utf8(cargo_toml_bytes.clone())?;

        let source = String::from_utf8(source_bytes)?;
        let parsed = split_source(&source);

        debug!(
            "RepairState: split {} into {} declarations",
            source_file_name,
            parsed.len()
        );

        let declarations = parsed
            .into_iter()
            .map(|ParsedDeclaration { source, .. }| MutableDeclaration { source })
            .collect();

        Ok(RepairState {
            declarations,
            cargo_toml,
            source_file_name,
            line_index: None,
        })
    }

    /// Assemble all declarations into a single source string, joined by blank lines.
    pub fn assemble_source(&self) -> String {
        self.declarations
            .iter()
            .map(|d| d.source.trim_end_matches('\n'))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Generate a context string with all declarations stubbed out (function bodies replaced
    /// with `{ todo!() }`, types and statics kept in full).
    ///
    /// This represents the current state of the whole file's interface, and is passed to the fix
    /// LLM as reference context for every repair call within a single iteration.
    pub fn interface_context(&self) -> String {
        self.declarations
            .iter()
            .map(|d| stub_declaration(&d.source))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Build a line-index that maps `(line_start, line_end, decl_index)` for every
    /// declaration in the *currently assembled* source.  All line numbers are 1-indexed.
    ///
    /// The separator between consecutive declarations is `"\n\n"` (one blank line).
    fn rebuild_line_index(&mut self) {
        let mut result = Vec::with_capacity(self.declarations.len());
        let mut current_line: usize = 1;

        for (i, decl) in self.declarations.iter().enumerate() {
            let trimmed = decl.source.trim_end_matches('\n');
            let line_count = trimmed.lines().count().max(1);
            let start = current_line;
            let end = current_line + line_count - 1;
            result.push((start, end, i));
            // Next declaration starts after one blank separator line.
            current_line = end + 2;
        }

        self.line_index = Some(result);
    }

    /// Get the line index, rebuilding it if invalidated.
    fn get_line_index(&mut self) -> &Vec<(usize, usize, usize)> {
        if self.line_index.is_none() {
            self.rebuild_line_index();
        }
        self.line_index.as_ref().unwrap()
    }

    /// Invalidate the line index, to be called after any update to declarations.
    fn invalidate_line_index(&mut self) {
        self.line_index = None;
    }

    /// Return the index of the declaration that owns `line` (1-indexed).
    /// Returns `None` if `line` falls in a separator or is out of range.
    /// Rebuilds the internal line index if it has been invalidated.
    pub fn find_declaration_for_line(&mut self, line: usize) -> Option<usize> {
        self.get_line_index()
            .iter()
            .find(|&&(start, end, _)| line >= start && line <= end)
            .map(|&(_, _, decl_idx)| decl_idx)
    }

    /// Replace declaration `decl_idx` with `new_source`.  Invalidates the line index.
    pub fn update_declaration(&mut self, decl_idx: usize, new_source: String) {
        if let Some(d) = self.declarations.get_mut(decl_idx) {
            d.source = new_source.trim_end_matches('\n').to_string();
            self.invalidate_line_index();
        }
    }

    /// Construct a fresh `CargoPackage` from the current repair state.
    pub fn to_cargo_package(&self) -> Result<CargoPackage, Box<dyn std::error::Error>> {
        let source = self.assemble_source();
        let mut dir = RawDir::default();
        dir.set_file("Cargo.toml", self.cargo_toml.clone().into_bytes())?;
        dir.set_file(&self.source_file_name, source.into_bytes())?;
        Ok(CargoPackage { dir })
    }
}

/// Produce a stub version of a declaration string: function bodies are replaced with
/// `{ todo!() }` so the context string is compact; all other items are kept verbatim.
fn stub_declaration(source: &str) -> String {
    let Ok(file) = syn::parse_file(source) else {
        return source.trim_end_matches('\n').to_string();
    };
    let stubbed_items: Vec<syn::Item> = file.items.into_iter().map(stub_item).collect();
    let stub_file = syn::File {
        shebang: None,
        attrs: vec![],
        items: stubbed_items,
    };
    prettyplease::unparse(&stub_file)
        .trim_end_matches('\n')
        .to_string()
}

/// Replace the body of a function (or the methods inside an `impl` block) with `{ todo!() }`.
fn stub_item(item: syn::Item) -> syn::Item {
    match item {
        syn::Item::Fn(mut f) => {
            f.block = Box::new(syn::parse_quote!({ todo!() }));
            syn::Item::Fn(f)
        }
        syn::Item::Impl(mut impl_block) => {
            impl_block.items = impl_block
                .items
                .into_iter()
                .map(|impl_item| match impl_item {
                    syn::ImplItem::Fn(mut method) => {
                        method.block = syn::parse_quote!({ todo!() });
                        syn::ImplItem::Fn(method)
                    }
                    other => other,
                })
                .collect();
            syn::Item::Impl(impl_block)
        }
        other => other,
    }
}
