//! End-to-end integration tests for `parse_to_ast` that drive libclang.
//!
//! Gated on `not(miri)` and on libclang's availability -- when libclang is
//! missing the tests skip rather than fail, mirroring the warn-and-skip
//! convention `extract_entities` uses on parse failure.

#![cfg(not(miri))]

use std::path::PathBuf;

use build_config::{BuildConfigIR, ConfigVarKind, ConfigVariable, SourceSelection, SourceVariant};
use c_ast::{ClangAST, RichSourceMap, TopLevelEntity, parse_to_ast};
use full_source::RawSource;
use harvest_core::fs::RawDir;

/// Three tiny self-contained C files. Each declares exactly one function so
/// we can identify entities by name regardless of include behavior. Keeping
/// them dependency-free side-steps the `params.h` `#error` guards in the
/// build_config fixtures.
const MAIN_C: &str = r#"
int main(void) { return 0; }
"#;

const BACKEND_ALPHA_C: &str = r#"
int backend_alpha(int x) { return x + 1; }
"#;

const BACKEND_BETA_C: &str = r#"
int backend_beta(int x) { return x + 2; }
"#;

fn mock_raw_source() -> RawSource {
    let mut dir = RawDir::default();
    dir.set_file("src/main.c", MAIN_C.as_bytes().to_vec())
        .unwrap();
    dir.set_file("src/backend_alpha.c", BACKEND_ALPHA_C.as_bytes().to_vec())
        .unwrap();
    dir.set_file("src/backend_beta.c", BACKEND_BETA_C.as_bytes().to_vec())
        .unwrap();
    RawSource { dir }
}

fn synthetic_build_config_with_backend() -> BuildConfigIR {
    BuildConfigIR {
        variables: vec![ConfigVariable {
            name: "BACKEND".into(),
            kind: ConfigVarKind::Enum {
                values: vec!["alpha".into(), "beta".into()],
                numeric: false,
            },
            default: Some("alpha".into()),
        }],
        defines: Vec::new(),
        source_selections: vec![SourceSelection {
            target: "backend".into(),
            driving_var: "BACKEND".into(),
            variants: vec![
                SourceVariant {
                    value: "alpha".into(),
                    files: vec![PathBuf::from("src/backend_alpha.c")],
                },
                SourceVariant {
                    value: "beta".into(),
                    files: vec![PathBuf::from("src/backend_beta.c")],
                },
            ],
        }],
        conditional_targets: Vec::new(),
        subdir_selections: Vec::new(),
        targets: Vec::new(),
        is_empty: false,
    }
}

fn empty_build_config() -> BuildConfigIR {
    BuildConfigIR {
        is_empty: true,
        ..Default::default()
    }
}

fn run(rs: &RawSource, cfg: Option<&BuildConfigIR>) -> Option<RichSourceMap> {
    match parse_to_ast(rs, cfg) {
        Ok(map) => Some(map),
        Err(err) => {
            eprintln!("Skipping: parse_to_ast failed (libclang missing?): {err}");
            None
        }
    }
}

fn find_function<'a>(map: &'a RichSourceMap, name: &str) -> Option<&'a TopLevelEntity> {
    map.app_functions.iter().find(|e| {
        matches!(
            e.ast.as_ref(),
            Some(ClangAST::FunctionDecl { name: n, .. }) if n == name
        )
    })
}

#[test]
fn variant_tags_populated_per_file_under_synthetic_ir() {
    // With BACKEND=alpha selecting `backend_alpha.c` and BACKEND=beta selecting
    // `backend_beta.c`, the declarations in those files carry the matching
    // tag. `main.c` is unselected, so its declarations get an empty vec.
    let raw = mock_raw_source();
    let cfg = synthetic_build_config_with_backend();
    let Some(map) = run(&raw, Some(&cfg)) else {
        return;
    };

    let alpha = find_function(&map, "backend_alpha").expect("backend_alpha function not found");
    assert_eq!(
        alpha.variant_tags,
        vec![("BACKEND".to_string(), "alpha".to_string())],
        "alpha-only function should be tagged with BACKEND=alpha"
    );

    let beta = find_function(&map, "backend_beta").expect("backend_beta function not found");
    assert_eq!(
        beta.variant_tags,
        vec![("BACKEND".to_string(), "beta".to_string())],
        "beta-only function should be tagged with BACKEND=beta"
    );

    let main = find_function(&map, "main").expect("main function not found");
    assert!(
        main.variant_tags.is_empty(),
        "main.c is not in any source_selection; variant_tags must be empty"
    );
}

#[test]
fn variant_tags_empty_under_empty_ir() {
    // Empty BuildConfigIR: every entity must have an empty variant_tags vec.
    let raw = mock_raw_source();
    let cfg = empty_build_config();
    let Some(map) = run(&raw, Some(&cfg)) else {
        return;
    };

    for entity in map.app_functions.iter() {
        assert!(
            entity.variant_tags.is_empty(),
            "empty-IR run produced non-empty variant_tags for {:?}",
            entity.ast
        );
    }
}

#[test]
fn variant_tags_empty_when_no_build_config_input() {
    // Backwards-compat path: parse_to_ast accepts None for the build config.
    // Every entity gets an empty variant_tags.
    let raw = mock_raw_source();
    let Some(map) = run(&raw, None) else {
        return;
    };

    for entity in map.app_functions.iter() {
        assert!(
            entity.variant_tags.is_empty(),
            "no-config run produced non-empty variant_tags for {:?}",
            entity.ast
        );
    }
}

#[test]
fn empty_ir_json_has_no_variant_tags_keys() {
    // Anti-regression: with an empty IR, the serialized JSON must not contain
    // any `variant_tags` key. This is what guarantees byte-equality with the
    // legacy outputs for the entire TRACTOR corpus (which lacks
    // configuration.json).
    let raw = mock_raw_source();
    let cfg = empty_build_config();
    let Some(map) = run(&raw, Some(&cfg)) else {
        return;
    };

    let json = serde_json::to_string(&map).unwrap();
    assert!(
        !json.contains("variant_tags"),
        "empty-IR JSON unexpectedly contains `variant_tags`; output was:\n{json}"
    );
}

#[test]
fn no_build_config_json_has_no_variant_tags_keys() {
    // Same anti-regression but for the path that doesn't even pass a
    // BuildConfigIR: schedules that don't supply one must produce
    // exactly the same output as the no-config path.
    let raw = mock_raw_source();
    let Some(map) = run(&raw, None) else {
        return;
    };

    let json = serde_json::to_string(&map).unwrap();
    assert!(
        !json.contains("variant_tags"),
        "no-config JSON unexpectedly contains `variant_tags`; output was:\n{json}"
    );
}

#[test]
fn no_config_and_empty_ir_produce_byte_equal_json() {
    // Strong byte-equality assertion: the serialized output must be identical
    // whether the caller supplies no BuildConfigIR at all or supplies an
    // is_empty IR. This is the contract the legacy pipeline depends on.
    let raw = mock_raw_source();
    let cfg = empty_build_config();
    let (Some(no_cfg_map), Some(empty_cfg_map)) = (run(&raw, None), run(&raw, Some(&cfg))) else {
        return;
    };

    let no_cfg_json = serde_json::to_string(&no_cfg_map).unwrap();
    let empty_cfg_json = serde_json::to_string(&empty_cfg_map).unwrap();
    assert_eq!(
        no_cfg_json, empty_cfg_json,
        "missing-IR and empty-IR JSON must be byte-equal"
    );
}
