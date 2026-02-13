use super::super::{DiagnosticsDir, Freezer, test_util::dir_has_entries};
use super::*;
use std::fs::{create_dir, write};
use std::io;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

/// Utility to easily build up a Dir with the given contents.
struct DirBuilder {
    diagnostics_dir: Arc<DiagnosticsDir>,
}

impl DirBuilder {
    pub fn new() -> DirBuilder {
        DirBuilder {
            diagnostics_dir: Arc::new(DiagnosticsDir::tempdir().unwrap()),
        }
    }

    pub fn add_dir<P: AsRef<Path>>(self, path: P) -> io::Result<DirBuilder> {
        create_dir(self.rel_path(path))?;
        Ok(self)
    }

    pub fn add_file<P: AsRef<Path>>(self, path: P, contents: &str) -> io::Result<DirBuilder> {
        write(self.rel_path(path), contents)?;
        Ok(self)
    }

    pub fn add_symlink<P: AsRef<Path>, T: AsRef<Path>>(
        self,
        path: P,
        target: T,
    ) -> io::Result<DirBuilder> {
        symlink(target, self.rel_path(path))?;
        Ok(self)
    }

    pub fn build(self) -> io::Result<Dir> {
        Ok(Freezer::new(self.diagnostics_dir)
            .freeze("")?
            .dir()
            .unwrap())
    }

    /// Appends `path` to diagnostics_dir's path.
    fn rel_path<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        PathBuf::from_iter([self.diagnostics_dir.path(), path.as_ref()])
    }
}

#[test]
fn get_nofollow() -> io::Result<()> {
    use GetNofollowError::*;
    let dir = DirBuilder::new()
        .add_dir("subdir")?
        .add_file("file", "")?
        .add_symlink("subdir/symlink", "..")?
        .build()?;
    assert_eq!(dir.get_nofollow("/absolute").err(), Some(LeavesDir));
    assert_eq!(dir.get_nofollow(".").err(), Some(NotADirectory));
    assert_eq!(dir.get_nofollow("subdir/..").err(), Some(NotADirectory));
    assert_eq!(dir.get_nofollow("file/c").err(), Some(NotADirectory));
    assert_eq!(
        dir.get_nofollow("subdir/symlink/file").err(),
        Some(NotADirectory)
    );
    assert_eq!(dir.get_nofollow("nonexistent").err(), Some(NotFound));
    assert_eq!(dir.get_nofollow("subdir/nonexistent").err(), Some(NotFound));
    assert!(dir_has_entries(&dir, &["file", "subdir"]));
    assert!(dir.get_nofollow("file").unwrap().file().is_some());
    assert!(dir_has_entries(
        &dir.get_nofollow("subdir").unwrap().dir().unwrap(),
        &["symlink"]
    ));
    let entry = dir.get_nofollow("subdir/symlink").unwrap();
    assert_eq!(entry.symlink().unwrap().contents(), "..");
    Ok(())
}
