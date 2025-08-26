pub mod c2rust_c_ast;
pub mod hc_ast;
pub mod raw_source;

use std::{any::Any, collections::BTreeMap, fmt::Display, path::Path};

/// Harvest Intermediate Representation
///
/// The Harvest IR is a collection of [Representation]s of a
/// program. Transformations of the IR may add or modify
/// representations.
pub struct HarvestIR {
    // The IR is composed of a set of [Representation]s identified by
    // some [Id] that is unique to that [Resentation] (at least for a
    // particular run of the pipeline). There may or may not be a
    // useful ordering for [Id]s, but for now using an ordered map at
    // least gives us a stable ordering when iterating, e.g. to print
    // the IR.
    representations: BTreeMap<Id, Box<dyn Representation>>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct Id(usize);

/// An abstract representation of a program
pub trait Representation: Display {
    fn as_any(&self) -> &dyn Any;
}

impl HarvestIR {
    /// Lift a source code project into a [HarvestIR].
    ///
    /// # Arguments
    ///
    /// * `path` - the [Path] to the source code project's root directory.
    ///
    /// # Examples
    ///
    /// ```
    /// # use harvest_ir::HarvestIR;
    /// # fn main() -> std::io::Result<()> {
    /// # let dir = tempdir::TempDir::new("harvest_test")?;
    /// # let path = dir.path();
    /// let harvest_ir = HarvestIR::from_raw_source(path)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_raw_source<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let dir = std::fs::read_dir(path)?;
        let root_dir = raw_source::RawDir::populate_from(dir)?;

	let mut result = HarvestIR { representations: Default::default() };
	result.representations.insert(Id(0), Box::new(root_dir));
        Ok(result)
    }

    pub fn get(&self, id: &Id) -> Option<&Box<dyn Representation>> {
        self.representations.get(id)
    }
}

impl Display for HarvestIR {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for r in self.representations.values() {
	    write!(f, "{r}")?;
        }
        Ok(())
    }
}
