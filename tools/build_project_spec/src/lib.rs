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

        // Autotools fallback: projects that ship no CMakeLists.txt (e.g.
        // libsodium) declare their targets in `Makefile.am` via Automake
        // primaries. `bin_PROGRAMS` => executable; `lib_LTLIBRARIES` /
        // `noinst_LTLIBRARIES` / `lib_LIBRARIES` => library. We scan every
        // Makefile.am in the tree because the primary that matters usually
        // lives in a subdirectory (src/, src/<name>/), not the project root.
        if let Some(kind) = project_kind_from_automake(repr) {
            return Ok(Box::new(ProjectSpec { kind }));
        }

        Err("Could not identify project kind from CMakeLists.txt / Makefile.am (or could not find either)".into())
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
    // Match after trimming leading whitespace: real projects (e.g. libpng)
    // indent `add_library(...)` / `add_executable(...)` inside `if()` blocks,
    // so a column-0-only prefix check would miss them.
    let starts = |prefix: &str| {
        text.lines()
            .any(|line| line.trim_start().starts_with(prefix))
    };
    // Library takes precedence when both appear: a project that builds a library
    // AND executables (e.g. libpng, which also builds test/tool binaries) is
    // fundamentally a library from the translation's perspective -- the exported
    // library surface is what downstream tools consume. Only classify as
    // Executable when there is no library target at all.
    if starts("add_library(") {
        Some(ProjectKind::Library)
    } else if starts("add_executable(") {
        Some(ProjectKind::Executable)
    } else {
        None
    }
}

/// Determine project kind by scanning every `Makefile.am` in the source tree
/// for Automake target primaries. This is the autotools analogue of
/// [`project_kind_from_cmakelists`], for projects that ship no CMakeLists.txt.
///
/// Detection markers (matched as the first non-whitespace token on a line,
/// tolerating the `NAME_PRIMARY = ...` assignment form):
/// - `bin_PROGRAMS`, `sbin_PROGRAMS`, `noinst_PROGRAMS` -> [`ProjectKind::Executable`]
/// - `lib_LTLIBRARIES`, `noinst_LTLIBRARIES`, `lib_LIBRARIES`,
///   `pkglib_LTLIBRARIES` -> [`ProjectKind::Library`]
///
/// Executable wins when both appear (a library that also builds a CLI driver
/// is exercised as an executable), matching the intent of the CMake matcher.
fn project_kind_from_automake(repr: &RawSource) -> Option<ProjectKind> {
    const EXE_PRIMARIES: &[&str] = &["bin_PROGRAMS", "sbin_PROGRAMS", "noinst_PROGRAMS"];
    const LIB_PRIMARIES: &[&str] = &[
        "lib_LTLIBRARIES",
        "noinst_LTLIBRARIES",
        "lib_LIBRARIES",
        "pkglib_LTLIBRARIES",
    ];

    let mut saw_library = false;
    for (path, contents) in repr.dir.files_recursive() {
        if path.file_name().and_then(|n| n.to_str()) != Some("Makefile.am") {
            continue;
        }
        let text = String::from_utf8_lossy(contents);
        for line in text.lines() {
            // The primary is the first whitespace-delimited token; an
            // assignment like `bin_PROGRAMS = foo` has it as token 0.
            let token = line.split_whitespace().next().unwrap_or("");
            if EXE_PRIMARIES.contains(&token) {
                return Some(ProjectKind::Executable);
            }
            if LIB_PRIMARIES.contains(&token) {
                saw_library = true;
            }
        }
    }
    saw_library.then_some(ProjectKind::Library)
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

    /// Real projects (e.g. libpng) INDENT `add_library`/`add_executable` inside
    /// `if()` blocks. Indented targets must still be detected.
    #[test]
    fn legacy_path_detects_indented_targets() {
        let cmakelists = b"if(PNG_SHARED)\n  add_library(png_shared SHARED x.c)\nendif()\n";
        assert!(
            matches!(
                project_kind_from_cmakelists(cmakelists),
                Some(ProjectKind::Library)
            ),
            "indented add_library must be detected"
        );
    }

    /// When a project declares BOTH a library and executables (libpng builds the
    /// png library plus pngtest/tool binaries), it is classified as a Library --
    /// the exported library surface is what the translation targets.
    #[test]
    fn legacy_path_library_wins_when_both_present() {
        let cmakelists = b"  add_library(png STATIC png.c)\n  add_executable(pngtest pngtest.c)\n";
        assert!(
            matches!(
                project_kind_from_cmakelists(cmakelists),
                Some(ProjectKind::Library)
            ),
            "library must take precedence when both target kinds are present"
        );
    }
}
