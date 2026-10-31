//! Lifts an existing on-disk Rust crate into a [`CargoPackage`] representation.
//!
//! This is the Rust-side analogue of [`load_raw_source::LoadRawSource`]: where
//! that tool loads a C project directory into a [`RawSource`], this loads a
//! already-translated Cargo package directory into a [`CargoPackage`]. It lets
//! the pipeline feed a prior translation into a later stage (e.g. the agentic
//! verify-and-fix stage) WITHOUT re-running translation.

use full_source::CargoPackage;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation, fs::RawDir};
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use tracing::info;

pub struct LoadCargoPackage {
    directory: PathBuf,
}

impl LoadCargoPackage {
    pub fn new(directory: &Path) -> LoadCargoPackage {
        LoadCargoPackage {
            directory: directory.into(),
        }
    }
}

impl Tool for LoadCargoPackage {
    fn name(&self) -> &'static str {
        "load_cargo_package"
    }

    fn run(
        self: Box<Self>,
        _context: RunContext,
        _inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let dir = read_dir(&self.directory).map_err(|e| {
            format!(
                "load_cargo_package: cannot read {}: {e}",
                self.directory.display()
            )
        })?;
        let (rawdir, directories, files) = RawDir::populate_from(dir)?;
        info!(
            "Loaded existing Cargo package: {directories} directories and {files} files from {}.",
            self.directory.display()
        );
        Ok(Box::new(CargoPackage { dir: rawdir }))
    }
}
