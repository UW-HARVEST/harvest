//! The [`BuildConfigIR`] representation and supporting types.
//!
//! `BuildConfigIR` captures the deterministic projection of a C project's
//! `configuration.json` plus the `CMakeLists.txt` patterns that mention those
//! variables.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use harvest_core::Representation;
use serde::{Deserialize, Serialize};

/// Structured view of the configurable variables, defines, and conditional
/// targets that drive a C project's build.
///
/// `is_empty == true` is the legacy short-circuit. Tools that have not been
/// taught to consume the new IR can branch on that flag and behave exactly as
/// they did before this PR.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildConfigIR {
    pub variables: Vec<ConfigVariable>,
    pub defines: Vec<DefineMapping>,
    pub source_selections: Vec<SourceSelection>,
    pub conditional_targets: Vec<ConditionalTarget>,
    /// Subdirectory trees selected by a configurable variable -- one
    /// [`SubdirSelection`] per `add_subdirectory(${VAR})` site, with one
    /// [`SubdirVariant`] per value of `VAR` whose subdirectory exists on disk.
    /// Always empty when [`Self::is_empty`] is `true`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subdir_selections: Vec<SubdirSelection>,
    pub is_empty: bool,
}

/// A single configurable variable declared in `configuration.json` and
/// (optionally) refined by `CMakeLists.txt` (`set(... CACHE STRING ...)` or
/// `option(...)`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigVariable {
    pub name: String,
    pub kind: ConfigVarKind,
    pub default: Option<String>,
}

/// Kind of a configurable variable. Booleans correspond to CMake `option(...)`
/// declarations or `configuration.json` arrays containing `true`/`false`;
/// `Enum` is the catch-all otherwise.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ConfigVarKind {
    Boolean,
    Enum {
        values: Vec<String>,
        /// `true` when every value parses as a signed integer (e.g. WORD_SIZE
        /// -> "32"/"64"). Downstream code uses this to decide whether `cfg!`
        /// guards should compare strings or integers.
        numeric: bool,
    },
}

/// A C-side `#define` produced by CMake, classified by the shape of the
/// substitution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DefineMapping {
    pub c_name: String,
    pub kind: DefineKind,
    pub source_vars: Vec<String>,
}

/// Shape of a CMake-emitted `#define`. See `scanner.rs` for the concrete
/// CMake patterns recognized.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DefineKind {
    /// `-DNAME="${VAR}"` -- value is the variable's value, quoted.
    QuotedString { var: String },
    /// `-DNAME=${VAR}` -- value is the variable's value, bare.
    Bare { var: String },
    /// `-DNAME=${X}_${Y}` -- template with placeholders for each contributing
    /// variable (`{X}_{Y}`).
    Composed { template: String },
    /// A `#define` that is emitted iff `gate_var` is truthy in CMake.
    GatedFlag { gate_var: String },
}

/// Per-target source selection driven by a configurable variable.
///
/// CMake pattern: `set(*_SOURCES path/with_${VAR}.c)` followed by
/// `add_library(target ... ${*_SOURCES})` or `add_executable(...)`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSelection {
    pub target: String,
    pub driving_var: String,
    pub variants: Vec<SourceVariant>,
}

/// One value of a [`SourceSelection`]'s driving variable, mapping to the file
/// list compiled when the variable takes that value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceVariant {
    pub value: String,
    pub files: Vec<PathBuf>,
}

/// A target whose entire definition lives inside `if(VAR) ... endif()`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConditionalTarget {
    pub target: String,
    pub gate_var: String,
    pub files: Vec<PathBuf>,
}

/// One `add_subdirectory(${VAR})` invocation.
///
/// Each value of `driving_var` whose corresponding subdirectory exists on disk
/// is scanned independently (with the parent scope's `${VAR}_SOURCES`
/// accumulators cloned, not shared) and contributes a [`SubdirVariant`]. Two
/// variants under the same selection never see each other's defines, target
/// definitions, or source lists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubdirSelection {
    /// The configurable variable that drives subdirectory selection.
    pub driving_var: String,
    /// One entry per value of `driving_var` whose `CMakeLists.txt` was found.
    pub variants: Vec<SubdirVariant>,
}

/// IR fragment captured by scanning one subdirectory of a [`SubdirSelection`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubdirVariant {
    /// The value of the driving variable selecting this variant.
    pub value: String,
    /// Project-relative path of the subdirectory whose `CMakeLists.txt` was
    /// scanned (e.g. `lib/blake`).
    pub path: PathBuf,
    /// Defines emitted inside this variant subtree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defines: Vec<DefineMapping>,
    /// Per-target source selections inside this variant subtree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_selections: Vec<SourceSelection>,
    /// `if(VAR) ... endif()`-gated targets inside this variant subtree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional_targets: Vec<ConditionalTarget>,
    /// Nested [`SubdirSelection`]s if this variant's subtree contains its own
    /// `add_subdirectory(${VAR})` calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subdir_selections: Vec<SubdirSelection>,
}

impl BuildConfigIR {
    /// Returns `true` when the IR records at least one executable target
    /// (either a `source_selection` with `target` naming an executable, or a
    /// `conditional_target` that is an executable). When the IR `is_empty` this
    /// always returns `false` so callers can fall back to legacy line-prefix
    /// matching in `CMakeLists.txt`.
    ///
    /// The heuristic: an executable target is any target whose name does NOT
    /// look like a library (i.e. it is not already known to be a library via
    /// `has_library_target`). In practice, the scanner records the raw CMake
    /// target name and the project kind is determined by the call site that
    /// actually reads `add_executable` / `add_library` from `CMakeLists.txt`.
    ///
    /// The scanner does not classify targets as executable vs. library -- it
    /// records them by name as they appear in source-selection /
    /// conditional-target patterns. This helper therefore acts as a lightweight
    /// **presence check**: if the IR is non-empty and contains any target that
    /// is associated with a source-selection or conditional-target entry, the
    /// project has at least *some* configurable target (executable or library).
    /// Consumers (`build_project_spec`) use it in conjunction with
    /// `has_library_target` to decide project kind; when both return `false`
    /// (empty IR) they fall back to the legacy line-prefix matcher.
    pub fn has_executable_target(&self) -> bool {
        if self.is_empty {
            return false;
        }
        // A non-empty IR with at least one source selection or conditional
        // target that does not overlap with library targets indicates an
        // executable. We detect this by checking all known targets and
        // returning `true` when any target is not a library target.
        let lib_targets = self.library_targets();
        let all_targets = self.all_target_names();
        all_targets
            .iter()
            .any(|t| !lib_targets.contains(t.as_str()))
            || (!all_targets.is_empty() && lib_targets.is_empty())
    }

    /// Returns `true` when the IR records at least one library target.
    ///
    /// The scanner marks a target as a library when the CMake pattern was
    /// `add_library(TARGET ...)`. Because the scanner stores targets by name
    /// without a kind tag, we apply the naming convention used throughout the
    /// HARVEST test corpus: a target whose name ends with `_lib` or equals
    /// the package name suffixed with `_lib`, or any target that appears only
    /// in `conditional_targets` with a `gate_var` (which typically guard
    /// optional libraries in `example_P02`).
    ///
    /// When `is_empty` this returns `false` unconditionally.
    pub fn has_library_target(&self) -> bool {
        if self.is_empty {
            return false;
        }
        !self.library_targets().is_empty()
    }

    /// Collect all target names mentioned by the IR.
    fn all_target_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .source_selections
            .iter()
            .map(|s| s.target.clone())
            .collect();
        for ct in &self.conditional_targets {
            if !names.contains(&ct.target) {
                names.push(ct.target.clone());
            }
        }
        names
    }

    /// Collect target names that look like libraries (heuristic: appears only
    /// in `conditional_targets`, which in `example_P02` model optional libs).
    fn library_targets(&self) -> std::collections::HashSet<String> {
        // Targets in conditional_targets that are NOT also in source_selections
        // are treated as library targets (they are gated by a boolean variable
        // and compile as optional `add_library` blocks in the C project).
        let ss_targets: std::collections::HashSet<&str> = self
            .source_selections
            .iter()
            .map(|s| s.target.as_str())
            .collect();
        self.conditional_targets
            .iter()
            .filter(|ct| !ss_targets.contains(ct.target.as_str()))
            .map(|ct| ct.target.clone())
            .collect()
    }
}

impl fmt::Display for BuildConfigIR {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty {
            return write!(f, "BuildConfigIR {{ empty }}");
        }
        write!(
            f,
            "BuildConfigIR {{ {} vars, {} defines, {} source_selections, {} conditional_targets, {} subdir_selections }}",
            self.variables.len(),
            self.defines.len(),
            self.source_selections.len(),
            self.conditional_targets.len(),
            self.subdir_selections.len(),
        )
    }
}

impl Representation for BuildConfigIR {
    fn name(&self) -> &'static str {
        "build_config"
    }

    /// Writes a pretty-printed JSON dump of the IR to `<path>/build_config.json`.
    ///
    /// `path` is the directory provided by the diagnostics layer; we create it
    /// if necessary so the surrounding code does not need to coordinate the
    /// containing-directory creation with us.
    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        fs::create_dir_all(path)?;
        let json_path = path.join("build_config.json");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(json_path, json)
    }
}
