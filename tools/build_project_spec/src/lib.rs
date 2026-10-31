use std::fmt::Display;

use build_config::BuildConfigIR;
use full_source::RawSource;
use harvest_core::Id;
use harvest_core::Representation;
use harvest_core::tools::{RunContext, Tool};

pub enum ProjectKind {
    Library,
    Executable,
}

impl Display for ProjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectKind::Library => write!(f, "Library"),
            ProjectKind::Executable => write!(f, "Executable"),
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
        // Get RawSource representation (inputs[0]) and BuildConfigIR (inputs[1]).
        let repr = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;
        let build_cfg = context
            .ir_snapshot
            .get::<BuildConfigIR>(inputs[1])
            .ok_or("No BuildConfigIR representation found in IR")?;

        // When the BuildConfigIR is non-empty, use the IR's target helpers to
        // determine project kind. The helpers are `false` for empty IRs so the
        // legacy line-prefix path below is taken for all existing TRACTOR cases
        // (invariant: byte-equal behavior on `is_empty`).
        if !build_cfg.is_empty {
            if build_cfg.has_executable_target() {
                return Ok(Box::new(ProjectSpec {
                    kind: ProjectKind::Executable,
                }));
            }
            if build_cfg.has_library_target() {
                return Ok(Box::new(ProjectSpec {
                    kind: ProjectKind::Library,
                }));
            }
            // Non-empty IR but no targets classified yet -- fall through to the
            // legacy matcher below. This can happen for projects that only
            // declare variables / defines without source-selections.
        }

        // Legacy path: line-prefix matching in CMakeLists.txt. Preserved
        // verbatim for `is_empty` IRs (the entire current TRACTOR corpus).
        if let Ok(cmakelists) = repr.dir.get_file("CMakeLists.txt")
            && let Some(kind) = project_kind_from_cmakelists(cmakelists)
        {
            return Ok(Box::new(ProjectSpec { kind }));
        }

        Err("Could not identify project kind from CMakeLists.txt (or could not find it)".into())
    }
}

/// Determine project kind by line-prefix matching in CMakeLists.txt content.
///
/// Returns `Some(ProjectKind::Executable)` when any line starts with
/// `add_executable(`, `Some(ProjectKind::Library)` when any line starts with
/// `add_library(`, and `None` otherwise.
///
/// This is the verbatim legacy matcher used for projects with an empty
/// `BuildConfigIR` -- behaviour must remain byte-equal to the original
/// line-prefix matching on main.
fn project_kind_from_cmakelists(cmakelists: &[u8]) -> Option<ProjectKind> {
    let text = String::from_utf8_lossy(cmakelists);
    if text.lines().any(|line| line.starts_with("add_executable(")) {
        Some(ProjectKind::Executable)
    } else if text.lines().any(|line| line.starts_with("add_library(")) {
        Some(ProjectKind::Library)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty-IR path is byte-equal to the legacy line-prefix matcher.
    /// An `add_executable(` prefix must yield `Executable`.
    #[test]
    fn legacy_path_detects_executable() {
        let cmakelists =
            b"cmake_minimum_required(VERSION 3.10)\nproject(foo)\nadd_executable(foo main.c)\n";
        let kind = project_kind_from_cmakelists(cmakelists);
        assert!(
            matches!(kind, Some(ProjectKind::Executable)),
            "add_executable( prefix must yield Executable"
        );
    }

    /// An `add_library(` prefix must yield `Library`.
    #[test]
    fn legacy_path_detects_library() {
        let cmakelists =
            b"cmake_minimum_required(VERSION 3.10)\nproject(foo)\nadd_library(foo STATIC foo.c)\n";
        let kind = project_kind_from_cmakelists(cmakelists);
        assert!(
            matches!(kind, Some(ProjectKind::Library)),
            "add_library( prefix must yield Library"
        );
    }

    /// When neither directive is present, `None` is returned (which maps to
    /// the error path in `run`).
    #[test]
    fn legacy_path_returns_none_when_neither() {
        let cmakelists = b"cmake_minimum_required(VERSION 3.10)\nproject(foo)\n";
        let kind = project_kind_from_cmakelists(cmakelists);
        assert!(kind.is_none(), "no add_* must yield None");
    }

    /// `add_executable(` must be a line-prefix match -- a line that contains
    /// it mid-line does NOT trigger the executable path.
    #[test]
    fn legacy_path_requires_line_prefix() {
        // "foo_add_executable(" starts with foo_, not add_executable(
        let cmakelists = b"# foo_add_executable(bar)\nadd_library(baz STATIC x.c)\n";
        let kind = project_kind_from_cmakelists(cmakelists);
        assert!(
            matches!(kind, Some(ProjectKind::Library)),
            "mid-line occurrence must not trigger executable"
        );
    }
}
