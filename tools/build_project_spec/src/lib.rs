use std::fmt::Display;
use std::path::Path;

use full_source::RawSource;
use harvest_core::Id;
use harvest_core::Representation;
use harvest_core::tools::{RunContext, Tool};

pub enum ProjectKind {
    Library,
    Executable,
    Configurable,
}

impl Display for ProjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectKind::Library => write!(f, "Library"),
            ProjectKind::Executable => write!(f, "Executable"),
            ProjectKind::Configurable => write!(f, "Configurable"),
        }
    }
}

pub struct ProjectSpec {
    pub kind: ProjectKind,
}

impl Display for ProjectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProjectSpec(kind={})", self.kind)
    }
}

impl Representation for ProjectSpec {
    fn name(&self) -> &'static str {
        "project_spec"
    }
}

/// Core detection logic shared by [`detect_project_kind`] and [`BuildProjectSpec::run`].
///
/// Examines the top-level `CMakeLists.txt` content to classify the project:
/// - `add_executable(` at line start -> Executable
/// - `add_library(` at line start -> Library
/// - `add_subdirectory(` at line start (with neither of the above) -> Configurable
///   (e.g. sphincs, where the real targets live in sub-CMakeLists)
fn kind_from_cmakelists(cmakelists: &[u8]) -> Option<ProjectKind> {
    let text = String::from_utf8_lossy(cmakelists);
    if text.lines().any(|l| l.starts_with("add_executable(")) {
        return Some(ProjectKind::Executable);
    }
    if text.lines().any(|l| l.starts_with("add_library(")) {
        return Some(ProjectKind::Library);
    }
    if text.lines().any(|l| l.starts_with("add_subdirectory(")) {
        return Some(ProjectKind::Configurable);
    }
    None
}

/// Detect the project kind from a source directory on the filesystem.
///
/// Useful for callers (e.g. the benchmark harness) that need the project kind
/// before or outside of a full tool pipeline.  Returns `None` if the kind
/// cannot be determined.
pub fn detect_project_kind(dir: &Path) -> Option<ProjectKind> {
    let cmakelists = std::fs::read(dir.join("CMakeLists.txt")).ok()?;
    kind_from_cmakelists(&cmakelists)
}

pub struct BuildProjectSpec;

impl Tool for BuildProjectSpec {
    fn name(&self) -> &'static str {
        "build_project_spec"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let repr = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;

        let cmakelists = repr.dir.get_file("CMakeLists.txt").map_err(
            |_| "Could not identify project kind from CMakeLists.txt (or could not find it)",
        )?;
        let kind = kind_from_cmakelists(cmakelists).ok_or(
            "CMakeLists.txt found but contains no add_executable, add_library, or add_subdirectory",
        )?;

        Ok(Box::new(ProjectSpec { kind }))
    }
}
