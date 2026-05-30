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

impl fmt::Display for BuildConfigIR {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
