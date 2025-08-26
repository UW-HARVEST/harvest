use std::{any::Any, collections::BTreeMap, ffi::OsString, fmt::Display, fs::ReadDir};

use crate::Representation;

/// A representation of a file-system directory entry.
pub enum RawEntry {
    Dir(RawDir),
    File(Vec<u8>),
}

impl RawEntry {
    fn dir(&self) -> Option<&RawDir> {
        match self {
            RawEntry::Dir(raw_dir) => Some(raw_dir),
            _ => None,
        }
    }

    fn file(&self) -> Option<&Vec<u8>> {
        match self {
            RawEntry::File(file) => Some(file),
            _ => None,
        }
    }
}

/// A representation of a file-system directory tree.
pub struct RawDir(pub BTreeMap<OsString, RawEntry>);

impl Display for RawDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.display(0, f)
    }
}

impl Representation for RawDir {
    fn as_any(&self) -> &dyn Any {
	self
    }
}

impl RawDir {
    /// Create a [RawDir] from a local file system directory
    ///
    /// # Arguments
    ///
    /// * `read_dir` - a [ReadDir] iterator over a file-system
    ///   directory.
    ///
    /// # Examples
    ///
    /// ```
    /// # use harvest_ir::raw_source::RawDir;
    /// # fn main() -> std::io::Result<()> {
    /// # let dir = tempdir::TempDir::new("harvest_test")?;
    /// # let path = dir.path();
    /// let raw_dir = RawDir::populate_from(std::fs::read_dir(path)?)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn populate_from(read_dir: ReadDir) -> std::io::Result<Self> {
        let mut result = BTreeMap::default();
        for entry in read_dir {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                let subdir = RawDir::populate_from(std::fs::read_dir(entry.path())?)?;
                result.insert(entry.file_name(), RawEntry::Dir(subdir));
            } else if metadata.is_file() {
                let contents = std::fs::read(entry.path())?;
                result.insert(entry.file_name(), RawEntry::File(contents));
            } else {
                unimplemented!("No support yet for symlinks in source project.");
            }
        }
        Ok(RawDir(result))
    }

    /// Print a representation of the directory to standard out.
    ///
    /// # Arguments
    ///
    /// * `level` - The level of this directory relative to the
    ///   root. Used to add padding to before entry names.
    pub fn display(&self, level: usize, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pad = "  ".repeat(level);
        for (name, entry) in self
            .0
            .iter()
            .filter_map(|(name, entry)| entry.dir().map(|e| (name, e)))
        {
            writeln!(f, "{pad}{}", name.to_string_lossy())?;
            entry.display(1, f)?;
        }

        for (name, entry) in self
            .0
            .iter()
            .filter_map(|(name, entry)| entry.file().map(|e| (name, e)))
        {
            writeln!(f, "{pad}{} ({}B)", name.to_string_lossy(), entry.len())?;
        }
        Ok(())
    }
}
