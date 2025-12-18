//! The Harvest Intermediate Representation ([HarvestIR]), types it depends on (e.g.
//! [Representation]), and utilities for working with them.

pub mod fs;
mod id;

pub use id::Id;
use std::any::Any;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

/// Harvest Intermediate Representation
///
/// The Harvest IR is a collection of [Representation]s of a
/// program. Transformations of the IR may add or modify
/// representations.
#[derive(Clone, Default)]
pub struct HarvestIR {
    // The IR is composed of a set of [Representation]s identified by
    // some [Id] that is unique to that [Resentation] (at least for a
    // particular run of the pipeline). There may or may not be a
    // useful ordering for [Id]s, but for now using an ordered map at
    // least gives us a stable ordering when iterating, e.g. to print
    // the IR.
    representations: BTreeMap<Id, Arc<dyn Representation>>,
}

/// An abstract representation of a program
pub trait Representation: Any + Display + Send + Sync {
    /// This representation's name. Used for diagnostics.
    fn name(&self) -> &'static str;

    /// Materialize the [Representation] to a directory at the
    /// provided `path`.
    ///
    /// Materializing stores an on-disk version of the
    /// [Representation]. The format is specific to each
    /// [Representation] variant.
    ///
    /// [Representation] provides an implementation that writes
    /// the Display output into a file. Representations may override
    /// materialize to provide a different output structure, such as
    /// a directory tree.
    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        writeln!(File::create_new(path)?, "{self}")
    }
}

impl HarvestIR {
    /// Insert `representation` with `id` to IR.
    pub fn insert_representation(&mut self, id: Id, representation: Box<dyn Representation>) -> Id {
        self.representations.insert(id, representation.into());
        id
    }

    /// Returns `true` if this `HarvestIR` contains a representation under ID `id`, `false`
    /// otherwise.
    pub fn contains_id(&self, id: Id) -> bool {
        self.representations.contains_key(&id)
    }

    /// Returns all contained Representations of the given type.
    pub fn get_all_by_representation<R: Representation>(&self) -> impl Iterator<Item = (Id, &R)> {
        // TODO: Add a `TypeId -> Id` map to HarvestIR that allows us to look these up without
        // scanning through all the other representations.
        self.representations
            .iter()
            .filter_map(|(&i, r)| <dyn Any>::downcast_ref(&**r).map(|r| (i, r)))
    }

    /// Returns the representation with the given ID, if it is of the expected type.
    pub fn get<R: Representation>(&self, id: Id) -> Option<&R> {
        self.representations
            .get(&id)
            .and_then(|r| <dyn Any>::downcast_ref(&**r))
    }

    /// Returns an iterator over the IDs and representations in this IR.
    pub fn iter(&self) -> impl Iterator<Item = (Id, &dyn Representation)> {
        self.representations.iter().map(|(&id, repr)| (id, &**repr))
    }
}

impl Display for HarvestIR {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, r) in self.representations.iter() {
            writeln!(f, "{i}: {r}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fmt::{self, Display, Formatter};

    /// A simple Representation that contains no data.
    pub struct EmptyRepresentation;
    impl Display for EmptyRepresentation {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            write!(f, "EmptyRepresentation")
        }
    }
    impl Representation for EmptyRepresentation {
        fn name(&self) -> &'static str {
            "empty"
        }
    }

    /// A Representation that contains only an ID number.
    #[derive(Debug, Eq, Hash, PartialEq)]
    pub struct IdRepresentation(pub usize);
    impl Display for IdRepresentation {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            write!(f, "IdRepresentation({})", self.0)
        }
    }
    impl Representation for IdRepresentation {
        fn name(&self) -> &'static str {
            "id"
        }
    }

    #[test]
    fn get_by_representation() {
        let mut ir = HarvestIR::default();
        ir.add_representation(Box::new(EmptyRepresentation));
        let b = ir.add_representation(Box::new(IdRepresentation(1)));
        ir.add_representation(Box::new(EmptyRepresentation));
        let d = ir.add_representation(Box::new(IdRepresentation(2)));
        assert_eq!(
            HashSet::from_iter(ir.get_by_representation::<IdRepresentation>()),
            HashSet::from([(b, &IdRepresentation(1)), (d, &IdRepresentation(2))])
        );
    }
}
