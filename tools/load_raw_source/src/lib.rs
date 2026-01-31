//! Lifts a source code project into a RawSource representation.

use full_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation, fs::RawDir};
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use tracing::info;

pub struct LoadRawSource {
    directory: PathBuf,
}

impl LoadRawSource {
    pub fn new(directory: &Path) -> LoadRawSource {
        LoadRawSource {
            directory: directory.into(),
        }
    }
}

impl Tool for LoadRawSource {
    fn name(&self) -> &'static str {
        "load_raw_source"
    }

    fn run(
        self: Box<Self>,
        _context: RunContext,
        _inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let dir = read_dir(self.directory.clone())?;
        let (rawdir, directories, files) = RawDir::populate_from(dir)?;
        info!(
            "Loaded {directories} directories and {files} files from {}.",
            self.directory.display()
        );
        Ok(Box::new(RawSource { dir: rawdir }))
    }
}
