use build_project_spec::{ProjectKind, ProjectSpec};
use full_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tracing::info;

/// The compiled C artifact produced by running CMake on the C source.
///
/// Exactly one of `so_path` or `exe_path` is `Some` depending on the project kind.
pub struct CArtifact {
    pub so_path: Option<PathBuf>,
    pub exe_path: Option<PathBuf>,
    _root: Option<Arc<TempDir>>,
}

impl std::fmt::Display for CArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match (&self.so_path, &self.exe_path) {
            (Some(p), _) => write!(f, "CArtifact(so={})", p.display()),
            (_, Some(p)) => write!(f, "CArtifact(exe={})", p.display()),
            _ => write!(f, "CArtifact(not applicable)"),
        }
    }
}

impl Representation for CArtifact {
    fn name(&self) -> &'static str {
        "c_artifact"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct BuildCArtifact;

impl Tool for BuildCArtifact {
    fn name(&self) -> &'static str {
        "build_c_artifact"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("build_c_artifact: no RawSource in IR")?;
        let project_spec = context
            .ir_snapshot
            .get::<ProjectSpec>(inputs[1])
            .ok_or("build_c_artifact: no ProjectSpec in IR")?;

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
            .map_err(|e| format!("build_c_artifact: failed to run cmake: {e}"))?;
        if !status.success() {
            return Err("build_c_artifact: cmake configure failed".into());
        }

        info!("Running cmake --build in {}", build_dir.display());
        let status = Command::new("cmake")
            .arg("--build")
            .arg(&build_dir)
            .status()
            .map_err(|e| format!("build_c_artifact: failed to run cmake --build: {e}"))?;
        if !status.success() {
            return Err("build_c_artifact: cmake build failed".into());
        }

        match project_spec.kind {
            ProjectKind::Library => {
                let so_path = find_shared_lib(&build_dir)
                    .ok_or("build_c_artifact: no .so found in build directory")?;
                info!("Found shared library: {}", so_path.display());
                Ok(Box::new(CArtifact {
                    so_path: Some(so_path),
                    exe_path: None,
                    _root: Some(root),
                }))
            }
            ProjectKind::Executable => {
                let exe_path = find_executable(&build_dir)
                    .ok_or("build_c_artifact: no executable found in build directory")?;
                info!("Found executable: {}", exe_path.display());
                Ok(Box::new(CArtifact {
                    so_path: None,
                    exe_path: Some(exe_path),
                    _root: Some(root),
                }))
            }
        }
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

/// Recursively walks `dir` and returns the first regular file that has the execute bit set
/// and no file extension. Skips `CMakeFiles` directories to avoid cmake build artifacts.
fn find_executable(dir: &Path) -> Option<PathBuf> {
    let read_dir = std::fs::read_dir(dir).ok()?;
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("CMakeFiles") {
                continue;
            }
            if let Some(found) = find_executable(&path) {
                return Some(found);
            }
        } else if path.extension().is_none()
            && let Ok(meta) = std::fs::metadata(&path)
            && meta.permissions().mode() & 0o111 != 0
        {
            return Some(path);
        }
    }
    None
}
