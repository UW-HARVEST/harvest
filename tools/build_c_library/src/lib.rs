use build_project_spec::{ProjectKind, ProjectSpec};
use full_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tracing::info;

/// The compiled C shared library artifact produced by running CMake on the C source.
/// `so_path` is `None` when the project is not a library (diff testing is skipped for
/// executables).
pub struct CLibraryArtifact {
    pub so_path: Option<PathBuf>,
    _root: Option<Arc<TempDir>>,
}

impl std::fmt::Display for CLibraryArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self.so_path {
            Some(p) => write!(f, "CLibraryArtifact({})", p.display()),
            None => write!(f, "CLibraryArtifact(not applicable)"),
        }
    }
}

impl Representation for CLibraryArtifact {
    fn name(&self) -> &'static str {
        "c_library_artifact"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct BuildCLibrary;

impl Tool for BuildCLibrary {
    fn name(&self) -> &'static str {
        "build_c_library"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("build_c_library: no RawSource in IR")?;
        let project_spec = context
            .ir_snapshot
            .get::<ProjectSpec>(inputs[1])
            .ok_or("build_c_library: no ProjectSpec in IR")?;

        if !matches!(project_spec.kind, ProjectKind::Library) {
            info!("build_c_library: project is not a library; skipping C build");
            return Ok(Box::new(CLibraryArtifact {
                so_path: None,
                _root: None,
            }));
        }

        let root = Arc::new(tempfile::tempdir()?);
        let src_dir = root.path().join("src");
        let build_dir = root.path().join("build");
        std::fs::create_dir_all(&src_dir)?;
        std::fs::create_dir_all(&build_dir)?;

        raw_source.dir.materialize(&src_dir)?;

        info!("Running cmake configure in {}", build_dir.display());
        let status = Command::new("cmake")
            .arg("-S")
            .arg(&src_dir)
            .arg("-B")
            .arg(&build_dir)
            .status()
            .map_err(|e| format!("build_c_library: failed to run cmake: {e}"))?;
        if !status.success() {
            return Err("build_c_library: cmake configure failed".into());
        }

        info!("Running cmake --build in {}", build_dir.display());
        let status = Command::new("cmake")
            .arg("--build")
            .arg(&build_dir)
            .status()
            .map_err(|e| format!("build_c_library: failed to run cmake --build: {e}"))?;
        if !status.success() {
            return Err("build_c_library: cmake build failed".into());
        }

        let so_path = find_shared_lib(&build_dir)
            .ok_or("build_c_library: no .so found in build directory")?;

        info!("Found shared library: {}", so_path.display());
        Ok(Box::new(CLibraryArtifact {
            so_path: Some(so_path),
            _root: Some(root),
        }))
    }
}

/// Recursively walks `dir` and returns the first file whose extension is exactly `so`.
/// Matches unversioned symlinks (`libfoo.so`) rather than versioned files (`libfoo.so.1.0.0`).
fn find_shared_lib(dir: &Path) -> Option<PathBuf> {
    let read_dir = std::fs::read_dir(dir).ok()?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_shared_lib(&path) {
                return Some(found);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("so") {
            return Some(path);
        }
    }
    None
}
