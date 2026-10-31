//! End-to-end test: drive [`apply`] with a synthetic [`BuildConfigIR`] that
//! matches the `example_P02` corpus, and assert that the resulting in-memory
//! [`CargoPackage`] contains the expected features, default selection,
//! `[package].build`, and `build.rs` contents.
//!
//! Pure in-memory -- no filesystem operations, so miri can run this freely.

use build_config::{BuildConfigIR, ConfigVarKind, ConfigVariable, DefineKind, DefineMapping};
use emit_build_features::apply;
use full_source::CargoPackage;
use harvest_core::fs::RawDir;

/// Build the canonical `example_P02`-style IR: 4 variables (APP_MODE,
/// BACKEND, ENABLE_EXTRA, WORD_SIZE), 4 defines (APP_MODE_STR, WORD_SIZE,
/// BUILD_PROFILE, ENABLE_EXTRA).
fn example_p02_ir() -> BuildConfigIR {
    BuildConfigIR {
        variables: vec![
            ConfigVariable {
                name: "APP_MODE".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["fast".into(), "safe".into()],
                    numeric: false,
                },
                default: Some("fast".into()),
            },
            ConfigVariable {
                name: "BACKEND".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["alpha".into(), "beta".into()],
                    numeric: false,
                },
                default: Some("alpha".into()),
            },
            ConfigVariable {
                name: "ENABLE_EXTRA".into(),
                kind: ConfigVarKind::Boolean,
                default: Some("false".into()),
            },
            ConfigVariable {
                name: "WORD_SIZE".into(),
                kind: ConfigVarKind::Enum {
                    values: vec!["32".into(), "64".into()],
                    numeric: true,
                },
                default: Some("32".into()),
            },
        ],
        defines: vec![
            DefineMapping {
                c_name: "APP_MODE_STR".into(),
                kind: DefineKind::QuotedString {
                    var: "APP_MODE".into(),
                },
                source_vars: vec!["APP_MODE".into()],
            },
            DefineMapping {
                c_name: "WORD_SIZE".into(),
                kind: DefineKind::Bare {
                    var: "WORD_SIZE".into(),
                },
                source_vars: vec!["WORD_SIZE".into()],
            },
            DefineMapping {
                c_name: "BUILD_PROFILE".into(),
                kind: DefineKind::Composed {
                    template: "{BACKEND}_{WORD_SIZE}".into(),
                },
                source_vars: vec!["BACKEND".into(), "WORD_SIZE".into()],
            },
            DefineMapping {
                c_name: "ENABLE_EXTRA".into(),
                kind: DefineKind::GatedFlag {
                    gate_var: "ENABLE_EXTRA".into(),
                    gate_value: None,
                },
                source_vars: vec!["ENABLE_EXTRA".into()],
            },
        ],
        source_selections: vec![],
        conditional_targets: vec![],
        subdir_selections: vec![],
        targets: vec![],
        is_empty: false,
    }
}

fn skeleton_package() -> CargoPackage {
    let mut dir = RawDir::default();
    dir.set_file(
        "Cargo.toml",
        b"[package]\nname = \"preset_skeleton\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            .to_vec(),
    )
    .unwrap();
    CargoPackage { dir }
}

/// Borrow the inner [`CargoPackage`] out of a [`Box<dyn Representation>`]
/// produced by [`apply`]. The `Representation` trait inherits `Any`, so the
/// usual `<dyn Any>::downcast_ref` machinery applies.
fn as_package(repr: &dyn harvest_core::Representation) -> &CargoPackage {
    <dyn std::any::Any>::downcast_ref::<CargoPackage>(repr).expect("apply must return CargoPackage")
}

#[test]
fn example_p02_produces_expected_features() {
    let pkg = skeleton_package();
    let ir = example_p02_ir();
    let out = apply(pkg, &ir).expect("apply succeeded");
    let pkg = as_package(&*out);

    let cargo_bytes = pkg.dir.get_file("Cargo.toml").expect("Cargo.toml present");
    let cargo = std::str::from_utf8(cargo_bytes).unwrap();

    for expected in [
        "APP_MODE_fast = []",
        "APP_MODE_safe = []",
        "BACKEND_alpha = []",
        "BACKEND_beta = []",
        "ENABLE_EXTRA = []",
        "WORD_SIZE_32 = []",
        "WORD_SIZE_64 = []",
    ] {
        assert!(
            cargo.contains(expected),
            "missing `{expected}` in Cargo.toml:\n{cargo}"
        );
    }
}

#[test]
fn example_p02_sets_default_features_to_recorded_defaults() {
    let pkg = skeleton_package();
    let ir = example_p02_ir();
    let out = apply(pkg, &ir).expect("apply succeeded");
    let pkg = as_package(&*out);

    let cargo_bytes = pkg.dir.get_file("Cargo.toml").expect("Cargo.toml present");
    let cargo = std::str::from_utf8(cargo_bytes).unwrap();

    // ENABLE_EXTRA default is false -> it must NOT appear in the default list.
    // APP_MODE default fast -> APP_MODE_fast.
    // BACKEND default alpha -> BACKEND_alpha.
    // WORD_SIZE default 32 -> WORD_SIZE_32.
    // Defaults are sorted.
    assert!(
        cargo.contains("default = [\"APP_MODE_fast\", \"BACKEND_alpha\", \"WORD_SIZE_32\"]"),
        "unexpected default list in Cargo.toml:\n{cargo}"
    );
}

#[test]
fn example_p02_sets_build_script() {
    let pkg = skeleton_package();
    let ir = example_p02_ir();
    let out = apply(pkg, &ir).expect("apply succeeded");
    let pkg = as_package(&*out);

    let cargo_bytes = pkg.dir.get_file("Cargo.toml").expect("Cargo.toml present");
    let cargo = std::str::from_utf8(cargo_bytes).unwrap();
    assert!(
        cargo.contains("build = \"build.rs\""),
        "missing [package].build in Cargo.toml:\n{cargo}"
    );
}

#[test]
fn example_p02_writes_build_rs() {
    let pkg = skeleton_package();
    let ir = example_p02_ir();
    let out = apply(pkg, &ir).expect("apply succeeded");
    let pkg = as_package(&*out);

    let build_rs = pkg.dir.get_file("build.rs").expect("build.rs present");
    let body = std::str::from_utf8(build_rs).unwrap();

    // The renderer's output must contain the expected emissions for every
    // configurable variable in the IR. Substring-match is sufficient.
    for needle in [
        "fn main() {",
        "cargo:rerun-if-changed=build.rs",
        "cargo:rustc-check-cfg=cfg(APP_MODE_fast, APP_MODE_safe)",
        "cargo:rustc-check-cfg=cfg(BACKEND_alpha, BACKEND_beta)",
        "cargo:rustc-check-cfg=cfg(WORD_SIZE_32, WORD_SIZE_64)",
        "cargo:rustc-cfg=APP_MODE_fast",
        "cargo:rustc-cfg=APP_MODE_safe",
        "cargo:rustc-cfg=BACKEND_alpha",
        "cargo:rustc-cfg=BACKEND_beta",
        "cargo:rustc-cfg=WORD_SIZE_32",
        "cargo:rustc-cfg=WORD_SIZE_64",
        "cargo:rustc-env=APP_MODE_STR=",
        "cargo:rustc-env=WORD_SIZE=",
        "cargo:rustc-env=BUILD_PROFILE=",
    ] {
        assert!(
            body.contains(needle),
            "missing `{needle}` in build.rs:\n{body}"
        );
    }
}

/// Byte-level snapshot of the emitted `Cargo.toml` for the `example_P02`
/// IR. The fixture under `tests/fixtures/example_p02/` is the formal model's
/// canonical output and is structurally equivalent to (but not byte-identical
/// to) the hand-written reference at `example_P02/config_c_rust/Rust/`.
/// Regressing against this snapshot means the renderer changed.
#[test]
fn example_p02_cargo_toml_byte_matches_snapshot() {
    let pkg = skeleton_package();
    let ir = example_p02_ir();
    let out = apply(pkg, &ir).expect("apply succeeded");
    let pkg = as_package(&*out);

    let actual = pkg.dir.get_file("Cargo.toml").expect("Cargo.toml present");
    let expected = include_bytes!("fixtures/example_p02/Cargo.toml");
    assert_eq!(
        std::str::from_utf8(actual).unwrap(),
        std::str::from_utf8(expected).unwrap(),
        "emitted Cargo.toml diverges from snapshot; refresh tests/fixtures/example_p02/Cargo.toml if intentional",
    );
}

/// Byte-level snapshot of the rendered `build.rs`. Same rationale as the
/// Cargo.toml snapshot.
#[test]
fn example_p02_build_rs_byte_matches_snapshot() {
    let pkg = skeleton_package();
    let ir = example_p02_ir();
    let out = apply(pkg, &ir).expect("apply succeeded");
    let pkg = as_package(&*out);

    let actual = pkg.dir.get_file("build.rs").expect("build.rs present");
    let expected = include_bytes!("fixtures/example_p02/build.rs");
    assert_eq!(
        std::str::from_utf8(actual).unwrap(),
        std::str::from_utf8(expected).unwrap(),
        "rendered build.rs diverges from snapshot; refresh tests/fixtures/example_p02/build.rs if intentional",
    );
}

/// Anti-regression: an empty `BuildConfigIR` must round-trip the `CargoPackage`
/// byte-for-byte through `apply`. This is the contract every project lacking
/// a `configuration.json` (i.e. the current TRACTOR corpus) relies on.
#[test]
fn empty_ir_round_trips_cargo_package() {
    let pkg = skeleton_package();
    let original = pkg
        .dir
        .get_file("Cargo.toml")
        .expect("Cargo.toml present")
        .to_vec();
    let ir = BuildConfigIR {
        is_empty: true,
        ..Default::default()
    };
    let out = apply(pkg, &ir).expect("apply succeeded on empty IR");
    let pkg = as_package(&*out);

    assert_eq!(pkg.dir.get_file("Cargo.toml").unwrap(), original.as_slice());
    assert!(
        pkg.dir.get_file("build.rs").is_err(),
        "empty IR must not produce a build.rs"
    );
}
