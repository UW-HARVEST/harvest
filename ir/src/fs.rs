//! Types representing a filesystem. Example use cases: representing a C source project, a Cargo
//! project, etc.

use std::collections::{BTreeMap, btree_map};
use std::ffi::OsString;
use std::fs::ReadDir;
use std::path::{Component, Path, PathBuf};

/// A representation of a file-system directory entry.
#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq))]
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
#[derive(Debug, Default)]
#[cfg_attr(test, derive(PartialEq))]
pub struct RawDir(BTreeMap<OsString, RawEntry>);

impl RawDir {
    /// Create a [RawDir] from a local file system directory
    ///
    /// Returns the [RawDir], number of directories and number of
    /// files, as a tuple.
    ///
    /// # Arguments
    ///
    /// * `read_dir` - a [ReadDir] iterator over a file-system
    ///   directory.
    ///
    /// # Examples
    ///
    /// ```
    /// # use harvest_core::fs::RawDir;
    /// # #[cfg(miri)] fn main() {}
    /// # #[cfg(not(miri))]
    /// # fn main() -> std::io::Result<()> {
    /// # let dir = tempfile::tempdir().unwrap();
    /// # let path = dir.path();
    /// let (raw_dir, num_dirs, num_files) = RawDir::populate_from(std::fs::read_dir(path)?)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn populate_from(read_dir: ReadDir) -> std::io::Result<(Self, usize, usize)> {
        let mut directories = 0;
        let mut files = 0;
        let mut result = BTreeMap::default();
        for entry in read_dir {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                let (subdir, dirs, fs) = RawDir::populate_from(std::fs::read_dir(entry.path())?)?;
                directories += dirs + 1;
                files += fs;
                result.insert(entry.file_name(), RawEntry::Dir(subdir));
            } else if metadata.is_file() {
                let contents = std::fs::read(entry.path())?;
                result.insert(entry.file_name(), RawEntry::File(contents));
                files += 1;
            } else {
                unimplemented!("No support yet for symlinks in source project.");
            }
        }
        Ok((RawDir(result), directories, files))
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

    /// Returns the path and contents of the files in this directory and its subdirectories. Paths
    /// are relative to this directory.
    pub fn files_recursive(&self) -> Vec<(PathBuf, &[u8])> {
        fn recurse<'s>(path: &mut PathBuf, dir: &'s RawDir, out: &mut Vec<(PathBuf, &'s [u8])>) {
            for (name, entry) in dir.0.iter() {
                match entry {
                    RawEntry::Dir(entry) => {
                        path.push(name);
                        recurse(path, entry, out);
                        path.pop();
                    }
                    RawEntry::File(contents) => {
                        let segments: [&Path; 2] = [path.as_ref(), name.as_ref()];
                        out.push((segments.iter().collect(), contents));
                    }
                }
            }
        }
        let mut out = vec![];
        recurse(&mut PathBuf::new(), self, &mut out);
        out
    }

    /// Gets the contents of a file at the given path. The file must
    /// exist. On success, returns a reference to file's contents.
    ///
    /// `path` must be a relative path. `..` is resolved lexically: it
    /// just removes the previously-specified directory (in general
    /// this isn't correct in the presence of symlinks, but `RawDir`
    /// does not support symlinks).
    pub fn get_file<P: AsRef<Path>>(&self, path: P) -> Result<&Vec<u8>, GetFileError> {
        // Determine which directories we need to descend into to reach the file (handling normal
        // directory names as well as . and ..), and split out the file name.
        let mut segments = vec![];
        // Whether the most-recently-processed entry can be a file.
        let mut last_can_be_file = true;
        for component in path.as_ref().components() {
            last_can_be_file = match component {
                Component::CurDir => false,
                Component::Normal(name) => {
                    segments.push(name);
                    true
                }
                Component::ParentDir => {
                    if segments.pop().is_none() {
                        return Err(GetFileError::OutsideDir);
                    }
                    false
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(GetFileError::AbsolutePath);
                }
            };
        }
        if !last_can_be_file {
            return Err(GetFileError::Directory);
        }
        let file_name = match segments.pop() {
            None => return Err(GetFileError::DoesNotExist),
            Some(empty) if empty.is_empty() => return Err(GetFileError::DoesNotExist),
            Some(name) => name,
        };

        let mut cur_dir = self;
        for component in segments {
            if let RawEntry::Dir(rd) = cur_dir.0.get(component).ok_or(GetFileError::DoesNotExist)? {
                cur_dir = rd;
            } else {
                return Err(GetFileError::UnderFile);
            }
        }
        if let RawEntry::File(v) = cur_dir.0.get(file_name).ok_or(GetFileError::DoesNotExist)? {
            Ok(v)
        } else {
            Err(GetFileError::Directory)
        }
    }

    /// Creates a new file at the given path. The file must not already exist. On success, returns
    /// a reference to the newly-added file.
    ///
    /// `path` must be a relative path. `..` is resolved lexically: it just removes the
    /// previously-specified directory (in general this isn't correct in the presence of symlinks,
    /// but `RawDir` does not support symlinks).
    pub fn set_file<P: AsRef<Path>>(
        &mut self,
        path: P,
        contents: Vec<u8>,
    ) -> Result<&mut Vec<u8>, SetFileError> {
        // Determine which directories we need to descend into to reach the file (handling normal
        // directory names as well as . and ..), and split out the file name.
        let mut segments = vec![];
        // Whether the most-recently-processed entry can be a file.
        let mut last_can_be_file = true;
        for component in path.as_ref().components() {
            last_can_be_file = match component {
                Component::CurDir => false,
                Component::Normal(name) => {
                    segments.push(name);
                    true
                }
                Component::ParentDir => {
                    if segments.pop().is_none() {
                        return Err(SetFileError::OutsideDir);
                    }
                    false
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(SetFileError::AbsolutePath);
                }
            };
        }
        if !last_can_be_file {
            return Err(SetFileError::Directory);
        }
        let filename = match segments.pop() {
            None => return Err(SetFileError::EmptyFileName),
            Some(empty) if empty.is_empty() => return Err(SetFileError::EmptyFileName),
            Some(name) => name.into(),
        };

        // Traverse through the directory tree to find the file entry.
        let mut cur_dir = self;
        for dir_name in segments {
            let RawDir(map) = cur_dir;
            let new_dir = map
                .entry(dir_name.into())
                .or_insert_with(|| RawEntry::Dir(RawDir::default()));
            let RawEntry::Dir(new_dir) = new_dir else {
                return Err(SetFileError::UnderFile);
            };
            cur_dir = new_dir;
        }
        let btree_map::Entry::Vacant(entry) = cur_dir.0.entry(filename) else {
            return Err(SetFileError::AlreadyExists);
        };
        let RawEntry::File(out) = entry.insert(RawEntry::File(contents)) else {
            panic!("RawEntry::File stopped being a file");
        };
        Ok(out)
    }

    /// Materializes the [RawDir] to the file system.
    ///
    /// `path` is a path to an empty or non-existent directory noting
    /// where the file system should be materialized to.
    pub fn materialize<P: AsRef<Path>>(&self, base_path: P) -> std::io::Result<()> {
        let base_path = base_path.as_ref();
        for (file_path, contents) in self.files_recursive().iter() {
            let dir_path = if let Some(parent) = file_path.parent() {
                base_path.join(parent)
            } else {
                base_path.into()
            };
            std::fs::create_dir_all(dir_path)?;
            std::fs::write(base_path.join(file_path), contents)?;
        }
        Ok(())
    }
}

/// Error type returned by [RawDir::set_file].
#[derive(Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum SetFileError {
    #[error("tried to set file at absolute path")]
    AbsolutePath,
    #[error("file already exists")]
    AlreadyExists,
    #[error("tried to set file at directory path")]
    Directory,
    #[error("empty file name")]
    EmptyFileName,
    #[error("tried to write file outside this directory")]
    OutsideDir,
    #[error("tried to set a file that is under another file")]
    UnderFile,
}

/// Error type returned by [RawDir::get_file].
#[derive(Debug, Eq, Hash, PartialEq, thiserror::Error)]
pub enum GetFileError {
    #[error("tried to get file at absolute path")]
    AbsolutePath,
    #[error("tried to get file at directory path")]
    Directory,
    #[error("tried to get a file outside this directory")]
    OutsideDir,
    #[error("tried to get a file that is under another file")]
    UnderFile,
    #[error("tried to get a file that does not exist")]
    DoesNotExist,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_recursive() {
        #[rustfmt::skip]
        let dir = RawDir([
            ("dir1".into(), RawEntry::Dir(RawDir([
                ("dir2".into(), RawEntry::Dir(RawDir([
                    ("file2.txt".into(), RawEntry::File(b"B".into())),
                ].into_iter().collect()))),
                ("file3.txt".into(), RawEntry::File(b"C".into())),
            ].into_iter().collect()))),
            ("file1.txt".into(), RawEntry::File(b"A".into())),
        ].into_iter().collect());
        // TODO: This comparison is sensitive to the order that files_recursive outputs its files,
        // which is not specified. We should either specify files_recursive's iteration order or
        // make this test insensitive to order.
        assert_eq!(
            dir.files_recursive(),
            [
                (PathBuf::from("dir1/dir2/file2.txt"), b"B".as_slice()),
                (PathBuf::from("dir1/file3.txt"), b"C".as_slice()),
                (PathBuf::from("file1.txt"), b"A".as_slice())
            ]
        );
    }

    #[test]
    fn set_file() {
        let mut root = RawDir::default();
        assert!(root.set_file("file1.txt", b"A".into()).is_ok());
        assert!(root.set_file("dir1/dir2/file2.txt", b"B".into()).is_ok());
        assert!(root.set_file("dir1/file3.txt", b"C".into()).is_ok());
        assert_eq!(
            root.set_file("/etc/passwd", b"D".into()),
            Err(SetFileError::AbsolutePath)
        );
        assert_eq!(
            root.set_file("dir1/file3.txt", b"E".into()),
            Err(SetFileError::AlreadyExists)
        );
        assert_eq!(
            root.set_file(".", b"F".into()),
            Err(SetFileError::Directory)
        );
        assert_eq!(
            root.set_file("dir1/dir2/..", b"G".into()),
            Err(SetFileError::Directory)
        );
        assert_eq!(
            root.set_file("", b"H".into()),
            Err(SetFileError::EmptyFileName)
        );
        assert_eq!(
            root.set_file("../", b"I".into()),
            Err(SetFileError::OutsideDir)
        );
        assert_eq!(
            root.set_file("dir1/../../", b"J".into()),
            Err(SetFileError::OutsideDir)
        );
        assert_eq!(
            root.set_file("file1.txt/file4.txt", b"K".into()),
            Err(SetFileError::UnderFile)
        );
        #[rustfmt::skip]
        assert_eq!(root, RawDir([
            ("dir1".into(), RawEntry::Dir(RawDir([
                ("dir2".into(), RawEntry::Dir(RawDir([
                    ("file2.txt".into(), RawEntry::File(b"B".into())),
                ].into_iter().collect()))),
                ("file3.txt".into(), RawEntry::File(b"C".into())),
            ].into_iter().collect()))),
            ("file1.txt".into(), RawEntry::File(b"A".into())),
        ].into_iter().collect()));
    }
}
