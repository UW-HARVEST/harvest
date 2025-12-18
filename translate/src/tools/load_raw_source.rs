//! Lifts a source code project into a RawSource representation.

use crate::tools::{RunContext, Tool};
use harvest_ir::Id;
use harvest_ir::{Representation, fs::RawDir};
use std::fs::read_dir;
use std::path::{Path, PathBuf};

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
        log::info!(
            "Loaded {directories} directories and {files} files from {}.",
            self.directory.display()
        );
        Ok(Box::new(RawSource { dir: rawdir }))
    }
}

/// A raw C project passed as input.
pub struct RawSource {
    pub dir: RawDir,
}

impl std::fmt::Display for RawSource {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Raw C source:")?;
        self.dir.display(0, f)
    }
}

impl Representation for RawSource {
    fn name(&self) -> &'static str {
        "RawSource"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        self.dir.materialize(path)
    }
}
