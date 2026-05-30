//! HARVEST tool: rewrites a [`CargoPackage`]'s `Cargo.toml` (and writes a
//! `build.rs`) so that the configurable variables surfaced in a
//! [`BuildConfigIR`] become Cargo features / build-script-emitted cfgs.
//!
//! Inputs (in order):
//! 1. [`CargoPackage`] id -- the translated Rust crate.
//! 2. [`BuildConfigIR`] id -- the structured projection of `configuration.json`.
//!
//! Output: a (possibly mutated) [`CargoPackage`].
//!
//! Behavior:
//! - If `BuildConfigIR.is_empty == true`, the input package is returned
//!   unchanged. This is the no-config short-circuit that lets the entire
//!   existing TRACTOR corpus continue to flow through the pipeline byte-for-byte.
//! - Otherwise, for each [`ConfigVariable`]:
//!     - `Boolean` => insert `<NAME> = []` into `[features]`.
//!     - `Enum { values }` => insert `<NAME>_<value> = []` for every value.
//! - `[features].default` is populated from each variable's recorded default.
//! - When the IR contains at least one non-trivial define or any enum variable,
//!   `[package].build = "build.rs"` is set and a `build.rs` rendered via
//!   [`build_config::render_build_rs`] is added to the package.

use std::collections::HashMap;

use build_config::{BuildConfigIR, ConfigVarKind, ConfigVariable, render_build_rs};
use full_source::CargoPackage;
use harvest_core::cargo_utils::CargoToml;
use harvest_core::config::unknown_field_warning;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

/// Tool config. No public knobs yet -- exists only to absorb unknown keys.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.emit_build_features", &self.unknown);
    }
}

/// The HARVEST `emit_build_features` tool.
pub struct EmitBuildFeatures;

impl Tool for EmitBuildFeatures {
    fn name(&self) -> &'static str {
        "emit_build_features"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        if let Some(raw) = context.config.tools.get("emit_build_features") {
            let config = Config::deserialize(raw)?;
            config.validate();
        }

        if inputs.len() < 2 {
            return Err(format!(
                "emit_build_features: expected 2 inputs (CargoPackage, BuildConfigIR), got {}",
                inputs.len()
            )
            .into());
        }

        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("emit_build_features: no CargoPackage at inputs[0]")?;
        let build_cfg = context
            .ir_snapshot
            .get::<BuildConfigIR>(inputs[1])
            .ok_or("emit_build_features: no BuildConfigIR at inputs[1]")?;

        if build_cfg.is_empty {
            debug!("emit_build_features: BuildConfigIR is_empty; passing CargoPackage through");
            return Ok(Box::new(cargo_package.clone()));
        }

        apply(cargo_package.clone(), build_cfg)
    }
}

/// Core, deterministic transform: takes ownership of a [`CargoPackage`] and
/// returns the mutated package. Factored out so unit tests can drive it
/// directly without constructing a [`RunContext`].
///
/// Returns the input unchanged when `cfg.is_empty == true`. The guard lives
/// here (not just in [`Tool::run`]) so callers cannot accidentally bypass the
/// no-config short-circuit that the entire TRACTOR corpus depends on.
pub fn apply(
    mut cargo_package: CargoPackage,
    cfg: &BuildConfigIR,
) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
    if cfg.is_empty {
        return Ok(Box::new(cargo_package));
    }

    let cargo_bytes = cargo_package
        .dir
        .get_file("Cargo.toml")
        .map_err(|e| format!("emit_build_features: Cargo.toml missing in CargoPackage: {e}"))?
        .to_vec();
    let mut cargo = CargoToml::from_bytes(&cargo_bytes)?;

    // 1. Surface every variable as one or more Cargo features.
    let mut feature_names: Vec<String> = Vec::new();
    for var in &cfg.variables {
        match &var.kind {
            ConfigVarKind::Boolean => feature_names.push(var.name.clone()),
            ConfigVarKind::Enum { values, .. } => {
                for v in values {
                    feature_names.push(format!("{}_{}", var.name, v));
                }
            }
        }
    }
    cargo.add_features(feature_names);

    // 2. Compute the default feature set: pick each variable's default value
    //    (or, for enums lacking one, fall back to the first listed value so
    //    builds still produce something).
    let defaults = default_features(&cfg.variables);
    cargo.set_default_features(&defaults);

    // 3. If anything in the IR needs a build script (any enum variable, or any
    //    non-trivial define), wire one up.
    let needs_build_rs = needs_build_script(cfg);
    if needs_build_rs {
        cargo.set_build_script("build.rs");
    }

    // 4. Write the updated Cargo.toml back into the in-memory package.
    let new_cargo = cargo.into_bytes();
    let cargo_slot = cargo_package
        .dir
        .get_file_mut("Cargo.toml")
        .map_err(|e| format!("emit_build_features: failed to mutate Cargo.toml: {e}"))?;
    *cargo_slot = new_cargo;

    // 5. Render and write build.rs (if needed). We tolerate a pre-existing
    //    build.rs by overwriting it in place.
    if needs_build_rs {
        let rendered = render_build_rs(cfg);
        let bytes = rendered.into_bytes();
        if cargo_package.dir.get_file("build.rs").is_ok() {
            let slot = cargo_package
                .dir
                .get_file_mut("build.rs")
                .map_err(|e| format!("emit_build_features: get_file_mut(build.rs): {e}"))?;
            *slot = bytes;
        } else if let Err(e) = cargo_package.dir.set_file("build.rs", bytes) {
            return Err(format!("emit_build_features: set_file(build.rs): {e}").into());
        }
    }

    Ok(Box::new(cargo_package))
}

/// True iff the IR requires a generated `build.rs`. Booleans alone do not -- the
/// Rust code can guard them with `#[cfg(feature = "...")]` directly.
fn needs_build_script(cfg: &BuildConfigIR) -> bool {
    let any_enum = cfg
        .variables
        .iter()
        .any(|v| matches!(v.kind, ConfigVarKind::Enum { .. }));
    let non_gated_define = cfg
        .defines
        .iter()
        .any(|d| !matches!(d.kind, build_config::DefineKind::GatedFlag { .. }));
    any_enum || non_gated_define
}

/// Translate every variable's default value into the matching feature name(s).
///
/// - Boolean -> bare name iff `default == "true"`.
/// - Enum -> `VAR_<default>`, falling back to the first listed value when no
///   default is recorded (matches CMake's behavior of using the first
///   cache value as the build default).
fn default_features(vars: &[ConfigVariable]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for var in vars {
        match &var.kind {
            ConfigVarKind::Boolean => {
                if matches!(var.default.as_deref(), Some("true")) {
                    out.push(var.name.clone());
                }
            }
            ConfigVarKind::Enum { values, .. } => {
                let pick = match var.default.as_deref() {
                    Some(d) if values.iter().any(|v| v == d) => d.to_string(),
                    Some(d) => {
                        warn!(
                            "emit_build_features: variable `{}` default `{}` not in declared values; \
                             falling back to first value",
                            var.name, d
                        );
                        values.first().cloned().unwrap_or_default()
                    }
                    None => values.first().cloned().unwrap_or_default(),
                };
                if pick.is_empty() {
                    warn!(
                        "emit_build_features: enum variable `{}` has no values; skipping default",
                        var.name
                    );
                    continue;
                }
                out.push(format!("{}_{}", var.name, pick));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use build_config::{ConfigVarKind, ConfigVariable, DefineKind, DefineMapping};

    fn boolean(name: &str, default: Option<&str>) -> ConfigVariable {
        ConfigVariable {
            name: name.into(),
            kind: ConfigVarKind::Boolean,
            default: default.map(str::to_owned),
        }
    }

    fn enum_var(name: &str, values: &[&str], default: Option<&str>) -> ConfigVariable {
        ConfigVariable {
            name: name.into(),
            kind: ConfigVarKind::Enum {
                values: values.iter().map(|v| (*v).to_owned()).collect(),
                numeric: values.iter().all(|v| v.parse::<i64>().is_ok()),
            },
            default: default.map(str::to_owned),
        }
    }

    #[test]
    fn default_features_picks_enum_default() {
        let v = vec![enum_var("BACKEND", &["alpha", "beta"], Some("alpha"))];
        assert_eq!(default_features(&v), vec!["BACKEND_alpha"]);
    }

    #[test]
    fn default_features_falls_back_to_first_value() {
        let v = vec![enum_var("BACKEND", &["alpha", "beta"], None)];
        assert_eq!(default_features(&v), vec!["BACKEND_alpha"]);
    }

    #[test]
    fn default_features_unknown_default_falls_back() {
        let v = vec![enum_var("BACKEND", &["alpha", "beta"], Some("gamma"))];
        assert_eq!(default_features(&v), vec!["BACKEND_alpha"]);
    }

    #[test]
    fn default_features_boolean_true_only() {
        let v = vec![
            boolean("ENABLE_EXTRA", Some("true")),
            boolean("DISABLED", Some("false")),
            boolean("UNSET", None),
        ];
        assert_eq!(default_features(&v), vec!["ENABLE_EXTRA"]);
    }

    #[test]
    fn default_features_sorted() {
        let v = vec![
            enum_var("B", &["x", "y"], Some("x")),
            enum_var("A", &["1", "2"], Some("2")),
        ];
        assert_eq!(default_features(&v), vec!["A_2", "B_x"]);
    }

    #[test]
    fn needs_build_script_for_enum() {
        let ir = BuildConfigIR {
            variables: vec![enum_var("BACKEND", &["alpha", "beta"], Some("alpha"))],
            ..Default::default()
        };
        assert!(needs_build_script(&ir));
    }

    #[test]
    fn needs_build_script_for_define() {
        let ir = BuildConfigIR {
            defines: vec![DefineMapping {
                c_name: "WORD_SIZE".into(),
                kind: DefineKind::Bare {
                    var: "WORD_SIZE".into(),
                },
                source_vars: vec!["WORD_SIZE".into()],
            }],
            ..Default::default()
        };
        assert!(needs_build_script(&ir));
    }

    #[test]
    fn no_build_script_for_pure_booleans() {
        let ir = BuildConfigIR {
            variables: vec![boolean("ENABLE_EXTRA", Some("false"))],
            ..Default::default()
        };
        assert!(!needs_build_script(&ir));
    }

    #[test]
    fn no_build_script_for_gated_define_only() {
        let ir = BuildConfigIR {
            variables: vec![boolean("ENABLE_EXTRA", Some("false"))],
            defines: vec![DefineMapping {
                c_name: "ENABLE_EXTRA".into(),
                kind: DefineKind::GatedFlag {
                    gate_var: "ENABLE_EXTRA".into(),
                },
                source_vars: vec!["ENABLE_EXTRA".into()],
            }],
            ..Default::default()
        };
        assert!(!needs_build_script(&ir));
    }
}
