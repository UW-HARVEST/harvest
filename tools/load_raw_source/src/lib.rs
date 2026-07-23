//! Lifts a source code project into a RawSource representation.

use full_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation, fs::RawDir};
use std::ffi::OsStr;
use std::fs::read_dir;
use std::path::{Path, PathBuf};
use tracing::info;

/// Directory names to exclude when lifting a C source project into the IR:
/// build-artifact directories that are not part of the source and can go stale
/// (e.g. a `build/` left from before a source change still holds object files
/// and a `.so` that no longer match the current sources).
fn is_build_artifact_dir(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "build" || name == "target" || name.starts_with("build-")
}

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
        let (rawdir, directories, files) =
            RawDir::populate_from_filtered(dir, &is_build_artifact_dir)?;
        info!(
            "Loaded {directories} directories and {files} files from {} (excluding build artifacts).",
            self.directory.display()
        );
        Ok(Box::new(RawSource { dir: rawdir }))
    }
}
