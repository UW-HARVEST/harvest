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

impl fmt::Display for BuildConfigIR {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BuildConfigIR {{ {} vars, {} defines, {} source_selections, {} conditional_targets }}",
            self.variables.len(),
            self.defines.len(),
            self.source_selections.len(),
            self.conditional_targets.len(),
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
