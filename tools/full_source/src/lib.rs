use std::path::Path;

use harvest_core::{Representation, fs::RawDir};

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

/// A cargo project representation (Cargo.toml, src/, etc).
pub struct CargoPackage {
    pub dir: RawDir,
}

impl std::fmt::Display for CargoPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Cargo package:")?;
        self.dir.display(0, f)
    }
}

impl Representation for CargoPackage {
    fn name(&self) -> &'static str {
        "CargoPackage"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        self.dir.materialize(path)
    }
}
