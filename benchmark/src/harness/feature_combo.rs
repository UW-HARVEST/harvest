//! Feature-combination enumeration for translated Rust crates.
//!
//! This module is responsible for:
//!
//! 1. Parsing `--feature-combos {default|all|N}` from the CLI.
//! 2. Reading the translated crate's `Cargo.toml [features]` block to discover
//!    which feature *groups* exist (each `VAR_value` feature belongs to the group
//!    `VAR`; boolean features are singleton groups).
//! 3. Enumerating (or sampling) the Cartesian product of groups.
//!
//! ## Feature group discovery
//!
//! `EmitBuildFeatures` writes features in the pattern `VAR_value` for
//! enum variables and bare `VAR` for booleans.  We reconstruct groups by
//! collecting all features that share the same prefix before the last `_`
//! component, *provided* more than one feature in the set shares that prefix.
//! Singleton features (no shared prefix) become their own groups with two
//! variants: enabled and disabled.
//!
//! The `default` feature key is not a group; it is the combination selected
//! when `--feature-combos default` is used.
//!
//! ## Combo string format
//!
//! Each combo is represented as a comma-separated, sorted list of the
//! enabled features, e.g. `"BACKEND_alpha,WORD_SIZE_64"`.  The special
//! string `"default"` is used when `--feature-combos default` is in effect.
//!
//! ## Sampling strategy (for `--feature-combos N`)
//!
//! When the full Cartesian product has P entries and N < P, we select
//! exactly N entries by taking evenly-spaced indices:
//!
//! ```text
//! indices = { floor(i * P / N) | i in 0..N }
//! ```
//!
//! This is deterministic (no RNG), covers the full range, and distributes
//! selections evenly.  For N >= P, all P entries are returned.

use crate::error::HarvestResult;
use harvest_core::cargo_utils::CargoToml;
use std::path::Path;
use std::str::FromStr;

/// Hard cap on the full Cartesian product size when `--feature-combos all` is used.
/// If the product exceeds this, `all` errors and the user must pass an explicit `N`.
const ALL_HARD_CAP: usize = 1024;

// ---------------------------------------------------------------------------
// CLI value type
// ---------------------------------------------------------------------------

/// The value for `--feature-combos`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureCombos {
    /// Only exercise the C build's default feature selection (no-op for crates
    /// without `[features]`).  This is the default and preserves backward
    /// compatibility with the existing TRACTOR corpus runs.
    Default,
    /// Exercise the full Cartesian product (capped at [`ALL_HARD_CAP`]).
    All,
    /// Sample at most `N` combos from the Cartesian product.
    N(usize),
}

impl FromStr for FeatureCombos {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "default" => Ok(FeatureCombos::Default),
            "all" => Ok(FeatureCombos::All),
            other => other
                .parse::<usize>()
                .map(|n| {
                    if n == 0 {
                        Err("--feature-combos N must be a positive integer".to_string())
                    } else {
                        Ok(FeatureCombos::N(n))
                    }
                })
                .map_err(|_| {
                    format!(
                        "invalid --feature-combos value '{}': expected 'default', 'all', or a positive integer",
                        other
                    )
                })?,
        }
    }
}

// ---------------------------------------------------------------------------
// Combo enumeration
// ---------------------------------------------------------------------------

/// A single feature combination: the set of features to pass via
/// `--no-default-features --features=<combo>`.
///
/// The `label` is the human-readable string used in `results.csv`
/// (`feature_combo` column).  The `features` list is sorted and passed
/// verbatim to `cargo build --features`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureCombo {
    /// Comma-separated sorted list of enabled feature names, or `"default"`.
    pub label: String,
    /// The `--features` argument (empty = use defaults), paired with
    /// `no_default_features` flag.
    pub features: Vec<String>,
    /// When `true`, pass `--no-default-features` to cargo.
    pub no_default_features: bool,
}

impl FeatureCombo {
    /// The single "default" combo: use Cargo's default features.
    pub fn default_combo() -> Self {
        FeatureCombo {
            label: "default".to_string(),
            features: Vec::new(),
            no_default_features: false,
        }
    }
}

/// Enumerate the feature combos to test for a translated crate.
///
/// Reads the `Cargo.toml` at `cargo_toml_path`, groups the `[features]`
/// entries into logical variables (enum groups vs. booleans), then expands
/// and optionally caps the Cartesian product.
///
/// Returns `Err` only when `mode == All` and the product would exceed
/// [`ALL_HARD_CAP`].  All other edge cases (no `[features]`, parse errors)
/// fall back gracefully to a single `default` combo.
pub fn enumerate_combos(
    cargo_toml_path: &Path,
    mode: &FeatureCombos,
) -> HarvestResult<Vec<FeatureCombo>> {
    // For the default mode, skip all parsing -- no behavior change.
    if matches!(mode, FeatureCombos::Default) {
        return Ok(vec![FeatureCombo::default_combo()]);
    }

    let cargo = match CargoToml::open(cargo_toml_path) {
        Ok(c) => c,
        Err(e) => {
            log::warn!(
                "feature_combo: failed to open {}: {}; falling back to default combo",
                cargo_toml_path.display(),
                e
            );
            return Ok(vec![FeatureCombo::default_combo()]);
        }
    };

    let all_features = cargo.feature_names();

    // No features -> single default combo.
    if all_features.is_empty() {
        return Ok(vec![FeatureCombo::default_combo()]);
    }

    // Group features into logical variables.
    let groups = group_features(&all_features);

    // Cartesian product.
    let product = cartesian_product(&groups);

    let cap = match mode {
        FeatureCombos::Default => unreachable!(),
        FeatureCombos::All => {
            if product.len() > ALL_HARD_CAP {
                return Err(format!(
                    "--feature-combos all: Cartesian product has {} combinations, which exceeds \
                     the hard cap of {}. Use --feature-combos N to sample instead.",
                    product.len(),
                    ALL_HARD_CAP
                )
                .into());
            }
            product.len()
        }
        FeatureCombos::N(n) => *n,
    };

    let selected = sample_combos(product, cap);

    Ok(selected
        .into_iter()
        .map(|features| {
            let mut sorted = features.clone();
            sorted.sort();
            let label = if sorted.is_empty() {
                "none".to_string()
            } else {
                sorted.join(",")
            };
            FeatureCombo {
                label,
                features: sorted,
                no_default_features: true,
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Feature grouping
// ---------------------------------------------------------------------------

/// A logical feature group: a set of mutually-exclusive values derived from
/// the same `VAR` prefix.
///
/// For enum variables (`VAR_value1`, `VAR_value2`, ...) the group contains all
/// enum variant feature names.  In each combo, exactly one member is selected.
///
/// For boolean variables (`VAR` with no underscore-suffix siblings) the group
/// has two variants: `[Some("VAR")]` (enabled) and `[None]` (disabled).
#[derive(Debug, Clone)]
pub struct FeatureGroup {
    /// Variant sets.  Each inner `Vec` is the list of features to enable for
    /// that variant.  Empty inner vec = no features enabled (boolean-off case).
    pub variants: Vec<Vec<String>>,
}

/// Reconstruct logical feature groups from a flat list of feature names.
///
/// Algorithm:
/// 1. Collect all features that share a common `PREFIX_` prefix into enum groups.
///    A prefix is only treated as an enum prefix when *two or more* features
///    share it (otherwise a single `FOO_BAR` is treated as a boolean).
/// 2. Features that do not belong to any multi-member enum group are treated
///    as boolean flags: each produces a two-variant group (on / off).
pub fn group_features(features: &[String]) -> Vec<FeatureGroup> {
    use std::collections::BTreeMap;

    // Count features per prefix (everything before the last '_').
    let mut prefix_count: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in features {
        if let Some(pos) = f.rfind('_') {
            let prefix = &f[..pos];
            prefix_count
                .entry(prefix.to_string())
                .or_default()
                .push(f.clone());
        }
    }

    // Collect the set of features claimed by an enum group (prefix with >= 2 members).
    let mut claimed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut groups: Vec<FeatureGroup> = Vec::new();

    // Sort prefix entries for deterministic ordering.
    let mut prefix_entries: Vec<(String, Vec<String>)> = prefix_count.into_iter().collect();
    prefix_entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (_, mut members) in prefix_entries {
        if members.len() < 2 {
            continue;
        }
        members.sort();
        for m in &members {
            claimed.insert(m.clone());
        }
        // Each member is one variant of the enum group.
        let variants: Vec<Vec<String>> = members.into_iter().map(|m| vec![m]).collect();
        groups.push(FeatureGroup { variants });
    }

    // Remaining features (not claimed by any enum group) are booleans.
    let mut booleans: Vec<String> = features
        .iter()
        .filter(|f| !claimed.contains(*f))
        .cloned()
        .collect();
    booleans.sort();
    for b in booleans {
        groups.push(FeatureGroup {
            variants: vec![vec![b], vec![]],
        });
    }

    groups
}

// ---------------------------------------------------------------------------
// Cartesian product
// ---------------------------------------------------------------------------

/// Compute the Cartesian product of all feature groups.
///
/// Returns a `Vec` of feature-name sets (each set is a `Vec<String>`).
/// An empty input produces `[vec![]]` (one combo: the empty set).
pub fn cartesian_product(groups: &[FeatureGroup]) -> Vec<Vec<String>> {
    if groups.is_empty() {
        return vec![vec![]];
    }
    let mut result: Vec<Vec<String>> = vec![vec![]];
    for group in groups {
        let mut next: Vec<Vec<String>> = Vec::new();
        for existing in &result {
            for variant in &group.variants {
                let mut combo = existing.clone();
                combo.extend_from_slice(variant);
                next.push(combo);
            }
        }
        result = next;
    }
    result
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// Select up to `cap` entries from `product` using evenly-spaced indices.
///
/// Sampling strategy: for a product of size P and a cap of N,
/// select indices `floor(i * P / N)` for `i` in `0..min(N, P)`.
/// This is deterministic, requires no RNG, and distributes the selected
/// entries evenly across the full range.
///
/// When `cap >= product.len()`, the full product is returned unchanged.
pub fn sample_combos(product: Vec<Vec<String>>, cap: usize) -> Vec<Vec<String>> {
    let p = product.len();
    if cap >= p {
        return product;
    }
    (0..cap).map(|i| product[i * p / cap].clone()).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- FeatureCombos::from_str ----

    #[test]
    fn parse_default() {
        assert_eq!(
            "default".parse::<FeatureCombos>().unwrap(),
            FeatureCombos::Default
        );
    }

    #[test]
    fn parse_all() {
        assert_eq!("all".parse::<FeatureCombos>().unwrap(), FeatureCombos::All);
    }

    #[test]
    fn parse_positive_integer() {
        assert_eq!("5".parse::<FeatureCombos>().unwrap(), FeatureCombos::N(5));
    }

    #[test]
    fn parse_zero_is_error() {
        assert!("0".parse::<FeatureCombos>().is_err());
    }

    #[test]
    fn parse_invalid_is_error() {
        assert!("foo".parse::<FeatureCombos>().is_err());
    }

    // ---- group_features ----

    #[test]
    fn group_enum_two_values() {
        let features = vec!["BACKEND_alpha".to_string(), "BACKEND_beta".to_string()];
        let groups = group_features(&features);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].variants.len(), 2);
        assert_eq!(groups[0].variants[0], vec!["BACKEND_alpha"]);
        assert_eq!(groups[0].variants[1], vec!["BACKEND_beta"]);
    }

    #[test]
    fn group_singleton_becomes_boolean() {
        // A single feature with an underscore but no sibling is a boolean.
        let features = vec!["ENABLE_EXTRA".to_string()];
        let groups = group_features(&features);
        assert_eq!(groups.len(), 1);
        // Two variants: on and off.
        assert_eq!(groups[0].variants.len(), 2);
        let on_variant = &groups[0].variants[0];
        let off_variant = &groups[0].variants[1];
        assert!(on_variant.contains(&"ENABLE_EXTRA".to_string()));
        assert!(off_variant.is_empty());
    }

    #[test]
    fn group_bare_boolean() {
        let features = vec!["MYFEATURE".to_string()];
        let groups = group_features(&features);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].variants.len(), 2);
    }

    #[test]
    fn group_mixed_enum_and_bool() {
        let features = vec![
            "BACKEND_alpha".to_string(),
            "BACKEND_beta".to_string(),
            "ENABLE_EXTRA".to_string(),
        ];
        let groups = group_features(&features);
        // One enum group (BACKEND) + one boolean group (ENABLE_EXTRA)
        assert_eq!(groups.len(), 2);
    }

    // ---- cartesian_product ----

    #[test]
    fn product_empty_groups() {
        let result = cartesian_product(&[]);
        assert_eq!(result, vec![vec![] as Vec<String>]);
    }

    #[test]
    fn product_two_groups_two_values_each() {
        // Mimics BACKEND={alpha,beta} x WORD_SIZE={32,64} => 4 combos.
        let groups = vec![
            FeatureGroup {
                variants: vec![
                    vec!["BACKEND_alpha".to_string()],
                    vec!["BACKEND_beta".to_string()],
                ],
            },
            FeatureGroup {
                variants: vec![
                    vec!["WORD_SIZE_32".to_string()],
                    vec!["WORD_SIZE_64".to_string()],
                ],
            },
        ];
        let product = cartesian_product(&groups);
        assert_eq!(product.len(), 4);
        // Check all expected combos present.
        let expected: Vec<Vec<String>> = vec![
            vec!["BACKEND_alpha".to_string(), "WORD_SIZE_32".to_string()],
            vec!["BACKEND_alpha".to_string(), "WORD_SIZE_64".to_string()],
            vec!["BACKEND_beta".to_string(), "WORD_SIZE_32".to_string()],
            vec!["BACKEND_beta".to_string(), "WORD_SIZE_64".to_string()],
        ];
        for exp in &expected {
            assert!(
                product.iter().any(|c| {
                    let mut sorted_c = c.clone();
                    sorted_c.sort();
                    let mut sorted_e = exp.clone();
                    sorted_e.sort();
                    sorted_c == sorted_e
                }),
                "missing combo {:?}",
                exp
            );
        }
    }

    #[test]
    fn product_deterministic_order() {
        // Two runs with same input must produce identical results.
        let groups = vec![
            FeatureGroup {
                variants: vec![vec!["A_x".to_string()], vec!["A_y".to_string()]],
            },
            FeatureGroup {
                variants: vec![vec!["B_1".to_string()], vec!["B_2".to_string()]],
            },
        ];
        let p1 = cartesian_product(&groups);
        let p2 = cartesian_product(&groups);
        assert_eq!(p1, p2);
    }

    // ---- sample_combos ----

    #[test]
    fn sample_no_cap_returns_all() {
        let product: Vec<Vec<String>> = (0..10).map(|i| vec![i.to_string()]).collect();
        let sampled = sample_combos(product.clone(), 20);
        assert_eq!(sampled, product);
    }

    #[test]
    fn sample_cap_gives_correct_count() {
        let product: Vec<Vec<String>> = (0..100).map(|i| vec![i.to_string()]).collect();
        let sampled = sample_combos(product, 7);
        assert_eq!(sampled.len(), 7);
    }

    #[test]
    fn sample_deterministic() {
        let product: Vec<Vec<String>> = (0..100).map(|i| vec![i.to_string()]).collect();
        let s1 = sample_combos(product.clone(), 10);
        let s2 = sample_combos(product, 10);
        assert_eq!(s1, s2);
    }

    #[test]
    fn sample_covers_first_and_last() {
        // First index is always 0*P/N = 0; last is floor((N-1)*P/N).
        let product: Vec<Vec<String>> = (0..100).map(|i| vec![i.to_string()]).collect();
        let sampled = sample_combos(product, 10);
        assert_eq!(sampled[0], vec!["0"]);
    }

    // ---- enumerate_combos: in-memory via temp Cargo.toml ----

    #[test]
    #[cfg_attr(miri, ignore)]
    fn enumerate_default_mode_skips_parsing() {
        // Even a non-existent path is fine when mode is Default.
        let combos = enumerate_combos(
            Path::new("/nonexistent/Cargo.toml"),
            &FeatureCombos::Default,
        )
        .unwrap();
        assert_eq!(combos.len(), 1);
        assert_eq!(combos[0].label, "default");
        assert!(!combos[0].no_default_features);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn enumerate_all_two_by_two() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
[package]
name = "test"
version = "0.1.0"
edition = "2021"

[features]
default = ["BACKEND_alpha", "WORD_SIZE_32"]
BACKEND_alpha = []
BACKEND_beta = []
WORD_SIZE_32 = []
WORD_SIZE_64 = []
"#
        )
        .unwrap();
        let combos = enumerate_combos(tmp.path(), &FeatureCombos::All).unwrap();
        assert_eq!(combos.len(), 4, "expected 4 combos, got {:?}", combos);
        // All combos must have no_default_features = true.
        assert!(combos.iter().all(|c| c.no_default_features));
        // Labels are sorted comma-separated.
        let labels: Vec<&str> = combos.iter().map(|c| c.label.as_str()).collect();
        for label in &labels {
            // Each label should contain exactly two features.
            assert_eq!(label.split(',').count(), 2, "unexpected label: {}", label);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn enumerate_sampling_cap() {
        use std::io::Write;
        // 4 combos from 2x2 features, cap at 2.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
[package]
name = "test"
version = "0.1.0"
edition = "2021"

[features]
default = ["A_x", "B_1"]
A_x = []
A_y = []
B_1 = []
B_2 = []
"#
        )
        .unwrap();
        let combos = enumerate_combos(tmp.path(), &FeatureCombos::N(2)).unwrap();
        assert_eq!(combos.len(), 2);
        // Deterministic: run twice gives same result.
        let combos2 = enumerate_combos(tmp.path(), &FeatureCombos::N(2)).unwrap();
        assert_eq!(combos, combos2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn enumerate_no_features_returns_default() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
        )
        .unwrap();
        let combos = enumerate_combos(tmp.path(), &FeatureCombos::All).unwrap();
        assert_eq!(combos.len(), 1);
        assert_eq!(combos[0].label, "default");
    }
}
