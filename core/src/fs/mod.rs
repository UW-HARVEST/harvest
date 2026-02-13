//! Filesystem abstractions. Used to represent directory trees, such as the input project or
//! lowered Rust source.
//!
//! # Freezing
//!
//! These types are read-only. To create them:
//!
//! 1. Create the file/directory/symlink in the diagnostic directory and populate it as intended.
//! 2. "freeze" the file using `Reporter::freeze_path` or `Scratch::freeze`.
//!
//! Freezing the contents will:
//! 1. Make the on-disk structures read-only. This applies recursively, but does not follow
//!    symlinks.
//! 2. Construct a [DirEntry] representing the on-disk structure.
//!
//! After you have frozen a filesystem object, it (and everything else frozen with it, if it is a
//! directory) must be left unchanged in the diagnostic directory. This is to avoid the need to
//! store the contents of files in memory.

mod dir;
mod freezer;

use crate::utils::{EmptyDirError, empty_writable_dir};
use std::collections::{BTreeMap, btree_map};
use std::ffi::OsString;
use std::fs::Permissions;
use std::fs::ReadDir;
use std::fs::canonicalize;
use std::fs::read_dir;
use std::fs::read_link;
use std::fs::remove_file;
use std::fs::set_permissions;
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Component, Path, PathBuf};
use std::str::Utf8Error;
use std::sync::Arc;
use tempfile::{TempDir, tempdir};
use thiserror::Error;

pub use dir::{Dir, DirEntry, ResolvedEntry};
pub(crate) use freezer::Freezer;

/// Utility to recursively delete a TempDir that contains read-only files and directories. Provided
/// to make it easier to delete the diagnostics directory (note that [DiagnosticsDir] automatically
/// deletes the diagnostic directory on drop if it is a TempDir).
pub fn delete_ro_tempdir(tempdir: TempDir) -> io::Result<()> {
    fn delete_contents(path: &mut PathBuf) -> io::Result<()> {
        set_permissions(&path, Permissions::from_mode(0o700))?;
        for entry in read_dir(&path)? {
            let entry = entry?;
            path.push(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                delete_contents(path)?;
            } else {
                if !file_type.is_symlink() {
                    set_permissions(&path, Permissions::from_mode(0o200))?;
                }
                remove_file(&path)?;
            }
            path.pop();
        }
        Ok(())
    }
    let mut path = tempdir.path().into();
    delete_contents(&mut path)?;
    tempdir.close()
}

/// Owns the diagnostics directory (if it is a temporary directory) and stores useful information
/// about it.
#[derive(Debug)]
pub struct DiagnosticsDir {
    path: PathBuf,
    // Owns the directory if it is temporary, otherwise is None.
    tempdir: Option<TempDir>,
}

impl Drop for DiagnosticsDir {
    fn drop(&mut self) {
        if let Some(tempdir) = self.tempdir.take() {
            let path = tempdir.path().to_owned();
            if let Err(error) = delete_ro_tempdir(tempdir) {
                panic!(
                    "Diagnostic directory cleanup failed. Path: {}. Error: {error}",
                    path.display()
                );
            };
        };
    }
}

impl DiagnosticsDir {
    /// Creates a new DiagnosticDir in a temporary directory.
    #[cfg(all(not(miri), test))]
    pub(crate) fn tempdir() -> Result<DiagnosticsDir, DiagnosticsDirNewError> {
        DiagnosticsDir::new(None, false)
    }

    /// Creates the DiagnosticDir instance. `path` is diagnostic_dir from the configuration, and
    /// `force` is force from the configuration.
    pub(crate) fn new(
        path: Option<&Path>,
        force: bool,
    ) -> Result<DiagnosticsDir, DiagnosticsDirNewError> {
        // We canonicalize the diagnostics path because it will be used to construct paths that are
        // passed as to external commands (as command-line arguments), and the canonicalized path
        // is probably the most compatible representation.
        let (path, tempdir) = match path {
            None => {
                let tempdir = tempdir()?;
                (canonicalize(tempdir.path())?, Some(tempdir))
            }
            Some(path) => {
                empty_writable_dir(path, force)?;
                (canonicalize(path)?, None)
            }
        };
        Ok(DiagnosticsDir { path, tempdir })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Error type returned by DiagnosticsDir::new.
#[derive(Debug, Error)]
pub enum DiagnosticsDirNewError {
    #[error("empty directory error")]
    EmptyDir(#[from] EmptyDirError),
    #[error("I/O error")]
    IoError(#[from] io::Error),
}

// Note: File and TextFile are internally Arc<> to a single shared type. That way, the UTF-8-ness
// of the file can be shared between the copies, because it is computed lazily.
/// A frozen file. A file can be a valid UTF-8, in which case it is considered a text file, or not
/// UTF-8, in which case it is not. A [File] can be converted into a [TextFile] using [TryFrom] if
/// the file is valid UTF-8.
#[derive(Clone, Debug)]
pub struct File {
    // TODO: Implement
}

impl File {
    /// Freezes the given file and returns a new File object referring to it. Note that this is for
    /// internal use by the diagnostics system; other code should use `Reporter::freeze` to create
    /// a File.
    fn new(
        diagnostics_dir: Arc<DiagnosticsDir>,
        path: PathBuf,
        permissions: Permissions,
    ) -> io::Result<File> {
        let _ = diagnostics_dir; // TODO: Remove once File is implemented.
        // Remove write permissions from user and all permissions from group + other. Leave execute
        // permission for user as-is.
        set_permissions(path, Permissions::from_mode(permissions.mode() & 0o500))?;
        Ok(File {})
    }

    /// Returns this file's contents as a byte array.
    pub fn bytes(&self) -> Arc<[u8]> {
        todo!()
    }

    /// Returns true if this file is UTF-8 (in which case it can be converted into a TextFile),
    /// false otherwise.
    pub fn is_utf8(&self) -> bool {
        todo!()
    }

    /// Returns the path to this file (or one instance thereof) in the diagnostic directory.
    pub fn path(&self) -> PathBuf {
        todo!()
    }
}

impl From<TextFile> for File {
    fn from(file: TextFile) -> File {
        let _ = file;
        todo!()
    }
}

/// A frozen UTF-8 file.
#[derive(Clone, Debug)]
pub struct TextFile {
    // TODO: Implement
}

impl TextFile {
    /// Returns this file's contents as a byte array.
    pub fn bytes(&self) -> Arc<[u8]> {
        self.str().into()
    }

    /// Returns the path to this file (or one instance thereof) in the diagnostic directory.
    pub fn path(&self) -> PathBuf {
        todo!()
    }

    /// Returns this file's contents as a str.
    pub fn str(&self) -> Arc<str> {
        todo!()
    }
}

impl TryFrom<File> for TextFile {
    type Error = Utf8Error;
    fn try_from(file: File) -> Result<TextFile, Utf8Error> {
        let _ = file;
        todo!()
    }
}

/// A symlink that has been frozen. Note that the thing it points to is not frozen; in fact it may
/// not exist or may be entirely outside the diagnostics directory.
#[derive(Clone, Debug)]
pub struct Symlink {
    // The path contained by this symlink.
    contents: Arc<Path>,
}

impl Symlink {
    /// Creates a new Symlink representing the file at the given path. This is for internal use by
    /// the diagnostics system; tool code should use [Reporter::freeze] or Scratch::freeze to
    /// create a symlink.
    fn new<P: AsRef<Path>>(path: P) -> io::Result<Symlink> {
        // Symlink permissions cannot be changed, so just create the Symlink object.
        Ok(Symlink {
            contents: read_link(path)?.into(),
        })
    }

    /// Returns this symlink's target path.
    pub fn contents(&self) -> &Path {
        &self.contents
    }

    /// Writes this symlink into the filesystem at the given path.
    pub fn write_rw<P: AsRef<Path>>(&self, path: P) -> io::Result<()> {
        symlink(&self.contents, path)
    }
}

// TODO: Remove; RawEntry is being replaced by DirEntry.
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
// TODO: Removed; RawDir is being replaced by Dir.
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
// TODO: Remove when no longer needed.
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
// TODO: Remove when no longer needed.
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

#[cfg(all(not(miri), test))]
mod test_util {
    use super::Dir;
    use std::collections::HashSet;

    /// Checks whether a Dir has entries with the given names
    pub fn dir_has_entries(dir: &Dir, names: &[&str]) -> bool {
        let entries: Result<HashSet<_>, _> = dir.entries().map(|(n, _)| n.into_string()).collect();
        let Ok(entries) = entries else { return false };
        let expected: HashSet<_> = names.iter().map(|&n| ToOwned::to_owned(n)).collect();
        entries == expected
    }
}

#[cfg(all(not(miri), test))]
mod tests {
    use super::*;
    use std::fs::{create_dir, read, write};

    /// Warning: If this test fails, check your /tmp. There's probably a bunch of leftover
    /// temporary directories there that you will need to manually delete. Sorry.
    #[test]
    fn delete_ro_tempdir_test() {
        let dir = tempdir().unwrap();
        // To verify that delete_ro_tempdir does not follow symlinks out of the directory it is
        // deleting, we create a *second* temporary directory and create symlinks into it.
        let other_dir = tempdir().unwrap();
        let other_file = PathBuf::from_iter([other_dir.path(), "other_file".as_ref()]);
        let other_subdir = PathBuf::from_iter([other_dir.path(), "other_subdir".as_ref()]);
        let other_symlink = PathBuf::from_iter([other_dir.path(), "other_symlink".as_ref()]);
        write(&other_file, "other_file_contents").unwrap();
        create_dir(&other_subdir).unwrap();
        symlink("other_file", &other_symlink).unwrap();
        // Populate dir/
        let subdir = PathBuf::from_iter([dir.path(), "subdir".as_ref()]);
        let subdir_file = PathBuf::from_iter([dir.path(), "subdir_file".as_ref()]);
        let subdir_symlink = PathBuf::from_iter([dir.path(), "subdir_symlink".as_ref()]);
        let file_symlink = PathBuf::from_iter([dir.path(), "file_symlink".as_ref()]);
        let dir_symlink = PathBuf::from_iter([dir.path(), "dir_symlink".as_ref()]);
        create_dir(&subdir).unwrap();
        write(&subdir_file, "subdir_file_contents").unwrap();
        symlink(other_symlink.canonicalize().unwrap(), &subdir_symlink).unwrap();
        symlink(other_file.canonicalize().unwrap(), &file_symlink).unwrap();
        symlink(other_subdir.canonicalize().unwrap(), &dir_symlink).unwrap();
        // Wipe out the permissions of everything under dir/ (except the symlinks, as changing the
        // permissions of a symlink changes its target's permissions).
        set_permissions(subdir_file, Permissions::from_mode(0o000)).unwrap();
        set_permissions(subdir, Permissions::from_mode(0o000)).unwrap();
        set_permissions(&dir, Permissions::from_mode(0o000)).unwrap();
        // Perform the deletion.
        delete_ro_tempdir(dir).unwrap();
        // Verify other_dir was unchanged.
        assert_eq!(read(other_file).unwrap(), b"other_file_contents");
        assert_eq!(read_dir(other_subdir).unwrap().count(), 0);
        assert_eq!(
            read_link(other_symlink).unwrap(),
            AsRef::<Path>::as_ref("other_file")
        );
    }

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
