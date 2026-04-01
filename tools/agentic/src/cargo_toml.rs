//! Helpers for reading, modifying, and writing Cargo.toml files.
//!
//! These are agentic-workflow-specific transformations that go beyond what `harvest_core::cargo_utils`
//! provides. In particular, the kiro-cli agent often produces Cargo manifests that need structural
//! normalization (workspace guard, binary/library naming, feature selection) before downstream tools
//! can consume them.

use std::fs;
use std::path::Path;

use toml_edit::{Array, DocumentMut, Item, Table};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// In-memory handle to a Cargo.toml file that supports incremental edits and a final write-back.
pub struct CargoToml {
    doc: DocumentMut,
    path: std::path::PathBuf,
}

impl CargoToml {
    /// Opens and parses a Cargo.toml at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let doc: DocumentMut = content
            .parse()
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        Ok(Self {
            doc,
            path: path.to_path_buf(),
        })
    }

    /// Writes the in-memory document back to disk.
    pub fn save(&self) -> Result<()> {
        fs::write(&self.path, self.doc.to_string())?;
        Ok(())
    }

    /// Sets `[lib]` with the given `name` and `crate-type = ["cdylib"]`.
    pub fn set_lib(&mut self, name: &str) {
        let lib = self
            .doc
            .entry("lib")
            .or_insert_with(|| Item::Table(Table::new()));
        if let Some(t) = lib.as_table_mut() {
            t.insert("name", toml_edit::value(name));
            let mut arr = Array::new();
            arr.push("cdylib");
            t.insert("crate-type", toml_edit::value(arr));
        }
    }

    /// Ensures there is a `[[bin]]` entry with `name = "driver"`.
    pub fn set_bin_driver(&mut self) {
        if let Some(bins) = self
            .doc
            .get_mut("bin")
            .and_then(|b| b.as_array_of_tables_mut())
            && let Some(bin) = bins.iter_mut().next()
        {
            bin.insert("name", toml_edit::value("driver"));
            return;
        }
        let mut bin = Table::new();
        bin.insert("name", toml_edit::value("driver"));
        bin.insert("path", toml_edit::value("src/main.rs"));
        let mut arr = toml_edit::ArrayOfTables::new();
        arr.push(bin);
        self.doc.insert("bin", Item::ArrayOfTables(arr));
    }

    /// Removes all `[[bin]]` sections.
    pub fn remove_bin(&mut self) {
        self.doc.remove("bin");
    }

    /// Adds an empty `[workspace]` section if not already present. This prevents Cargo from
    /// searching parent directories for a workspace root.
    pub fn add_workspace(&mut self) {
        if self.doc.get("workspace").is_none() {
            self.doc.insert("workspace", Item::Table(Table::new()));
        }
    }

    /// Replaces `default = [...]` under `[features]`. Currently unused; reserved for Phase 2
    /// multi-config support.
    #[allow(dead_code)]
    pub fn set_default_features(&mut self, features: &[String]) {
        let feat = self
            .doc
            .entry("features")
            .or_insert_with(|| Item::Table(Table::new()));
        if let Some(t) = feat.as_table_mut() {
            let mut arr = Array::new();
            for f in features {
                arr.push(f.as_str());
            }
            t.insert("default", toml_edit::value(arr));
        }
    }
}

/// Removes `src/main.rs` and the `tests/` directory from a translated library project.
pub fn strip_for_lib(translated_rust_dir: &Path) -> Result<()> {
    let main_rs = translated_rust_dir.join("src/main.rs");
    if main_rs.exists() {
        fs::remove_file(&main_rs)?;
    }
    let tests_dir = translated_rust_dir.join("tests");
    if tests_dir.exists() {
        fs::remove_dir_all(&tests_dir)?;
    }
    Ok(())
}
