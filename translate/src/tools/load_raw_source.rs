//! Lifts a source code project into a RawSource representation.

use crate::tools::{MightWriteContext, MightWriteOutcome, RunContext, Tool};
use harvest_core::{Representation, fs::RawDir};
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

    // LoadRawSource will create a new representation, not modify an existing
    // one.
    fn might_write(&mut self, _context: MightWriteContext) -> MightWriteOutcome {
        MightWriteOutcome::Runnable([].into())
    }

    fn run(self: Box<Self>, context: RunContext) -> Result<(), Box<dyn std::error::Error>> {
        let dir = read_dir(self.directory.clone())?;
        let (rawdir, directories, files) = RawDir::populate_from(dir)?;
        info!(
            "Loaded {directories} directories and {files} files from {}.",
            self.directory.display()
        );
        context
            .ir_edit
            .add_representation(Box::new(RawSource { dir: rawdir }));
        Ok(())
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
