//! Integration tests for the `build_config` scanner. Each test loads a
//! hermetic fixture from `tests/fixtures/<case>/` and asserts the
//! produced [`BuildConfigIR`].

#![cfg(not(miri))]

use std::path::{Path, PathBuf};

use build_config::{
    BuildConfigIR, ConfigVarKind, ConfigVariable, DefineKind, DefineMapping, SourceSelection,
    SourceVariant, scan,
};
use harvest_core::fs::RawDir;

fn fixture(case: &str) -> RawDir {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(case);
    let read_dir =
        std::fs::read_dir(&path).unwrap_or_else(|e| panic!("read_dir({}): {}", path.display(), e));
    let (dir, _, _) =
        RawDir::populate_from(read_dir).expect("RawDir::populate_from on fixture failed");
    dir
}

fn pick<'a>(vars: &'a [ConfigVariable], name: &str) -> &'a ConfigVariable {
    vars.iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("missing variable {name}"))
}

fn pick_define<'a>(defines: &'a [DefineMapping], c_name: &str) -> &'a DefineMapping {
    defines
        .iter()
        .find(|d| d.c_name == c_name)
        .unwrap_or_else(|| panic!("missing define {c_name}"))
}

#[test]
fn config_notests_variables_have_expected_kinds() {
    let dir = fixture("config_notests");
    let ir = scan(&dir);
    assert!(!ir.is_empty, "config_notests should not be is_empty");
    assert_eq!(ir.variables.len(), 4, "expected 4 vars in {ir}");

    let app_mode = pick(&ir.variables, "APP_MODE");
    assert_eq!(
        app_mode.kind,
        ConfigVarKind::Enum {
            values: vec!["fast".into(), "safe".into()],
            numeric: false,
        }
    );
    assert_eq!(app_mode.default.as_deref(), Some("fast"));

    let backend = pick(&ir.variables, "BACKEND");
    assert_eq!(
        backend.kind,
        ConfigVarKind::Enum {
            values: vec!["alpha".into(), "beta".into()],
            numeric: false,
        }
    );
    assert_eq!(backend.default.as_deref(), Some("alpha"));

    let word_size = pick(&ir.variables, "WORD_SIZE");
    assert_eq!(
        word_size.kind,
        ConfigVarKind::Enum {
            values: vec!["32".into(), "64".into()],
            numeric: true,
        }
    );
    assert_eq!(word_size.default.as_deref(), Some("32"));

    let enable_extra = pick(&ir.variables, "ENABLE_EXTRA");
    assert_eq!(enable_extra.kind, ConfigVarKind::Boolean);
    assert_eq!(enable_extra.default.as_deref(), Some("false"));
}

#[test]
fn config_notests_defines_match_cmake_patterns() {
    let dir = fixture("config_notests");
    let ir = scan(&dir);

    let app_mode_str = pick_define(&ir.defines, "APP_MODE_STR");
    assert_eq!(
        app_mode_str.kind,
        DefineKind::QuotedString {
            var: "APP_MODE".into()
        }
    );
    assert_eq!(app_mode_str.source_vars, vec!["APP_MODE".to_string()]);

    let word_size = pick_define(&ir.defines, "WORD_SIZE");
    assert_eq!(
        word_size.kind,
        DefineKind::Bare {
            var: "WORD_SIZE".into()
        }
    );

    let build_profile = pick_define(&ir.defines, "BUILD_PROFILE");
    assert_eq!(
        build_profile.kind,
        DefineKind::Composed {
            template: "{BACKEND}_{WORD_SIZE}".into()
        }
    );
    // Order isn't load-bearing but the set should be exactly {BACKEND, WORD_SIZE}.
    let mut composed_vars = build_profile.source_vars.clone();
    composed_vars.sort();
    assert_eq!(
        composed_vars,
        vec!["BACKEND".to_string(), "WORD_SIZE".into()]
    );

    let enable_extra = pick_define(&ir.defines, "ENABLE_EXTRA");
    assert_eq!(
        enable_extra.kind,
        DefineKind::GatedFlag {
            gate_var: "ENABLE_EXTRA".into()
        }
    );
}

#[test]
fn config_notests_source_selection_for_backend() {
    let dir = fixture("config_notests");
    let ir = scan(&dir);
    assert_eq!(ir.source_selections.len(), 1);
    let SourceSelection {
        target,
        driving_var,
        variants,
    } = &ir.source_selections[0];
    assert_eq!(target, "backend");
    assert_eq!(driving_var, "BACKEND");

    let alpha_files: Vec<PathBuf> = vec![PathBuf::from("src/backend_alpha.c")];
    let beta_files: Vec<PathBuf> = vec![PathBuf::from("src/backend_beta.c")];
    assert_eq!(
        variants,
        &vec![
            SourceVariant {
                value: "alpha".into(),
                files: alpha_files,
            },
            SourceVariant {
                value: "beta".into(),
                files: beta_files,
            },
        ]
    );
}

#[test]
fn config_notests_conditional_target_extra() {
    let dir = fixture("config_notests");
    let ir = scan(&dir);
    assert_eq!(ir.conditional_targets.len(), 1, "{ir}");
    let target = &ir.conditional_targets[0];
    assert_eq!(target.target, "extra");
    assert_eq!(target.gate_var, "ENABLE_EXTRA");
    assert_eq!(target.files, vec![PathBuf::from("src/extra.c")]);
}

#[test]
fn config_tests_parses_same_shape_as_notests() {
    // The `config_tests` fixture differs from `config_notests` only in
    // `SHARED` vs `STATIC` and a couple of comment lines. The IR should be
    // identical aside from the variant order.
    let dir = fixture("config_tests");
    let ir = scan(&dir);
    assert!(!ir.is_empty);
    assert_eq!(ir.variables.len(), 4);
    assert_eq!(ir.source_selections.len(), 1);
    assert_eq!(ir.conditional_targets.len(), 1);
}

#[test]
fn missing_configuration_json_yields_is_empty() {
    let dir = fixture("noconfig");
    let ir = scan(&dir);
    assert!(ir.is_empty);
    assert!(ir.variables.is_empty());
    assert!(ir.defines.is_empty());
    assert!(ir.source_selections.is_empty());
    assert!(ir.conditional_targets.is_empty());
}

#[test]
fn ir_display_matches_summary_format() {
    let dir = fixture("config_notests");
    let ir = scan(&dir);
    let s = format!("{ir}");
    assert!(s.starts_with("BuildConfigIR {"), "unexpected display: {s}");
    assert!(s.contains("vars"));
    assert!(s.contains("defines"));
}

#[test]
fn ir_display_for_empty_ir() {
    let dir = fixture("noconfig");
    let ir = scan(&dir);
    assert_eq!(format!("{ir}"), "BuildConfigIR { empty }");
}

#[test]
fn materialize_writes_pretty_json() {
    use harvest_core::Representation as _;
    let dir = fixture("config_notests");
    let ir = scan(&dir);
    let tmp = tempfile::tempdir().unwrap();
    ir.materialize(tmp.path()).expect("materialize failed");
    let written: &Path = &tmp.path().join("build_config.json");
    let bytes = std::fs::read(written).expect("build_config.json missing");
    let parsed: BuildConfigIR =
        serde_json::from_slice(&bytes).expect("round-trip deserialize failed");
    assert_eq!(parsed, ir);
}
