mod annotations;
mod ast;
mod rsm;
mod utils;

use build_config::{BuildConfigIR, SubdirVariant};
use clang::{Clang as LibClang, Index};
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

pub use annotations::{EntityAnnotations, annotate_visibility};
pub use ast::ClangAST;
pub use rsm::{EntityKind, RichSourceMap, SourcePoint, SourceSpan, TopLevelEntity};

/// Lookup table from canonicalized absolute file path to the list of
/// `(driving_var, value)` tags that file participates in. Built once from
/// [`BuildConfigIR`] before parsing so per-entity lookup is a single hash hit.
type VariantTagMap = HashMap<PathBuf, Vec<(String, String)>>;

pub struct ParseToAst;

/// Build a [`VariantTagMap`] keyed by canonicalized absolute paths under
/// `src_root`. Entries are accumulated, so a file referenced by two distinct
/// IR fragments ends up with both tags.
///
/// Walks the IR recursively: at every [`SubdirSelection`](build_config::SubdirSelection)
/// the `(driving_var, variant.value)` pair is pushed onto an outer-tag
/// accumulator and the variant's interior is walked with that accumulator
/// active. Files in `SourceSelection`s receive the inner variant tag stacked
/// on top of all accumulated outer tags; files declared by plain
/// `add_executable` / `add_library` (i.e. those carried by `targets` or
/// `conditional_targets`) inside a subdirectory variant receive the outer
/// tags only. The flat-IR case (no `subdir_selections`) is byte-equivalent
/// to the pre-recursive implementation.
///
/// Files missing from disk are skipped (canonicalization failure) -- they
/// would not appear in any entity span either, so the asymmetry is harmless.
fn build_variant_tag_map(cfg: Option<&BuildConfigIR>, src_root: &Path) -> VariantTagMap {
    let mut map: VariantTagMap = HashMap::new();
    let Some(cfg) = cfg else { return map };
    if cfg.is_empty {
        return map;
    }
    walk_top(cfg, src_root, &mut map);
    map
}

/// Walk the top-level `BuildConfigIR`, then recurse into each
/// `SubdirSelection`. Tags from `source_selections` and `subdir_selections`
/// at this level have no outer accumulator yet.
fn walk_top(cfg: &BuildConfigIR, src_root: &Path, map: &mut VariantTagMap) {
    for selection in &cfg.source_selections {
        for variant in &selection.variants {
            for rel_path in &variant.files {
                add_tag(
                    map,
                    src_root,
                    rel_path,
                    &[],
                    Some((&selection.driving_var, &variant.value)),
                );
            }
        }
    }
    for ss in &cfg.subdir_selections {
        for sv in &ss.variants {
            let outer = [(ss.driving_var.clone(), sv.value.clone())];
            walk_subdir_variant(sv, src_root, &outer, map);
        }
    }
}

/// Walk one [`SubdirVariant`] with `outer_tags` accumulated from every
/// enclosing `SubdirSelection`. Every file declared in this variant's
/// `source_selections`, `conditional_targets`, or `targets` receives the
/// accumulated outer tags; files inside inner `source_selections` also pick
/// up their own `(driving_var, value)` tag.
fn walk_subdir_variant(
    sv: &SubdirVariant,
    src_root: &Path,
    outer_tags: &[(String, String)],
    map: &mut VariantTagMap,
) {
    for selection in &sv.source_selections {
        for variant in &selection.variants {
            for rel_path in &variant.files {
                add_tag(
                    map,
                    src_root,
                    rel_path,
                    outer_tags,
                    Some((&selection.driving_var, &variant.value)),
                );
            }
        }
    }
    for ct in &sv.conditional_targets {
        for rel_path in &ct.files {
            add_tag(map, src_root, rel_path, outer_tags, None);
        }
    }
    for target in &sv.targets {
        for rel_path in &target.files {
            add_tag(map, src_root, rel_path, outer_tags, None);
        }
    }
    for ss in &sv.subdir_selections {
        for nested in &ss.variants {
            let mut next: Vec<(String, String)> = outer_tags.to_vec();
            next.push((ss.driving_var.clone(), nested.value.clone()));
            walk_subdir_variant(nested, src_root, &next, map);
        }
    }
}

/// Append all `outer_tags` (then optionally one inner tag) to the entry for
/// `rel_path` in `map`. Canonicalizes the path against `src_root` so spans
/// looked up later hit the same key.
fn add_tag(
    map: &mut VariantTagMap,
    src_root: &Path,
    rel_path: &Path,
    outer_tags: &[(String, String)],
    inner: Option<(&str, &str)>,
) {
    let abs = src_root.join(rel_path);
    let key = abs.canonicalize().unwrap_or(abs);
    let entry = map.entry(key).or_default();
    for tag in outer_tags {
        entry.push(tag.clone());
    }
    if let Some((var, value)) = inner {
        entry.push((var.to_owned(), value.to_owned()));
    }
}

/// Lookup the variant tags for an entity span. Returns an empty `Vec` when
/// the file isn't recorded in any `SourceSelection`.
fn variant_tags_for(span: &SourceSpan, map: &VariantTagMap) -> Vec<(String, String)> {
    if map.is_empty() {
        return Vec::new();
    }
    let key = PathBuf::from(&span.file);
    let canonical = key.canonicalize().unwrap_or(key);
    map.get(&canonical).cloned().unwrap_or_default()
}

/// Utility function to generate libClang parser arguments based on the source root and file being parsed.
/// This includes standard flags, include paths, and language specification based on file extension.
fn generate_parse_args(src_root: &Path, rel_file: &Path) -> Vec<String> {
    let mut parser_arg_values = vec!["-std=gnu11".to_string()];
    parser_arg_values.push(format!("-I{}/include", src_root.to_string_lossy()));
    parser_arg_values.extend(
        utils::language_args_for_file(rel_file)
            .iter()
            .map(|s| s.to_string()),
    );
    parser_arg_values
}

/// Utility function to instantiate the libclang parser.
fn build_parser<'a>(index: &'a Index, src_root: &Path, rel_file: &Path) -> clang::Parser<'a> {
    let abs_file = src_root.join(rel_file);
    let parser_arg_values = generate_parse_args(src_root, rel_file);

    debug!(
        "Parsing {} with args: {:?}",
        rel_file.to_string_lossy(),
        parser_arg_values
    );

    let mut parser = index.parser(abs_file);
    parser.detailed_preprocessing_record(true);
    parser.arguments(&parser_arg_values);
    parser
}

/// Extract top-level entities from the translation unit.
/// This includes both entities that survive preprocessing (types, functions, globals) and preprocessor directives (includes, defines, compiler args).
fn extract_entities(
    parser: clang::Parser<'_>,
    variant_map: &VariantTagMap,
    out: &mut RichSourceMap,
) {
    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => {
            warn!("Skipping due to parse failure: {:?}", e);
            return;
        }
    };

    let root = tu.get_entity();

    for child in root.get_children() {
        // Ignore entities that we don't care about.
        let Some(decl_kind) = rsm::map_top_level_decl_kind(child.get_kind()) else {
            continue;
        };

        // Ignore imports
        if child.is_in_system_header() || utils::get_file_location(&child).is_none() {
            continue;
        }

        // Read the source text from the file
        let Some((span, source_text)) = utils::get_span_and_text(&child) else {
            continue;
        };

        // Extract the AST for this entity
        let ast = ast::ast_from_entity(decl_kind, &child);
        let variant_tags = variant_tags_for(&span, variant_map);
        out.push_entity(
            TopLevelEntity {
                kind: decl_kind,
                source_text,
                span,
                ast,
                annotations: EntityAnnotations::default(),
                sub_entities: Vec::new(),
                variant_tags,
            },
            &child,
        );
    }
}

impl Tool for ParseToAst {
    fn name(&self) -> &'static str {
        "parse_to_ast"
    }

    /// For each C and header file in the RawSource, parse it with libClang and extract top-level declarations into a RichSourceMap.
    /// Captures preprocessor information such as include paths and defines as well.
    ///
    /// Inputs:
    /// 1. [`RawSource`] id -- the C project to parse.
    /// 2. (optional) [`BuildConfigIR`] id -- drives per-entity `variant_tags`.
    ///    When absent or `is_empty`, every entity's `variant_tags` is the
    ///    empty vec and serialized output is byte-equal to the form
    ///    produced without a `BuildConfigIR` input.
    ///
    /// We deliberately keep file discovery extension-based even when a
    /// `BuildConfigIR` is supplied: the modular translator needs ASTs for
    /// every variant simultaneously so it can emit
    /// `#[cfg(<DRIVING_VAR>_<VALUE>)] mod <variant>;` per variant. The
    /// variant_tags are labels, not a filter.
    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let id = inputs[0];
        let rs = context
            .ir_snapshot
            .get::<RawSource>(id)
            .ok_or("No RawSource representation found in IR")?;

        // Second input is optional so old schedules (and tests that don't care
        // about variant tagging) keep working.
        let build_cfg: Option<&BuildConfigIR> = inputs
            .get(1)
            .and_then(|cfg_id| context.ir_snapshot.get::<BuildConfigIR>(*cfg_id));

        let map = parse_to_ast(rs, build_cfg)?;
        Ok(Box::new(map))
    }
}

/// Core deterministic transform: parse every C/header file in `rs` and
/// produce a [`RichSourceMap`]. Each entity is stamped with `variant_tags`
/// derived from `cfg` (empty when `cfg` is `None` or `is_empty`).
///
/// Factored out so callers can drive parsing without a full
/// [`RunContext`] / scheduler -- the integration tests use this path.
pub fn parse_to_ast(
    rs: &RawSource,
    cfg: Option<&BuildConfigIR>,
) -> Result<RichSourceMap, Box<dyn std::error::Error>> {
    let src_dir = tempfile::TempDir::new()?;
    rs.dir.materialize(src_dir.path())?;

    let variant_map = build_variant_tag_map(cfg, src_dir.path());

    let clang = LibClang::new().map_err(|e| format!("Failed to initialize libclang: {e}"))?;
    let index = Index::new(&clang, false, false);

    let mut out = RichSourceMap::new();

    for (rel_path, _) in rs.dir.files_recursive() {
        if utils::should_skip_path(&rel_path) {
            continue;
        }
        if !utils::is_c_or_header(&rel_path) {
            continue;
        }
        tracing::info!("Parsing file: {}", rel_path.to_string_lossy());
        let parser = build_parser(&index, src_dir.path(), &rel_path);
        extract_entities(parser, &variant_map, &mut out);
    }

    debug!(
        "Generated RichSourceMap:\n{}",
        serde_json::to_string_pretty(&out)?
    );
    Ok(out)
}

#[cfg(not(miri))]
#[cfg(test)]
mod tests {
    //! Unit tests for the variant-tag lookup. The libclang-driven entity
    //! extraction is covered by `tests/parse_to_ast.rs`; these tests focus on
    //! the pure-Rust lookup table that maps spans to tags.
    use super::*;
    use build_config::{ConfigVariable, SourceSelection, SourceVariant};
    use std::fs;

    fn span_for(path: &Path) -> SourceSpan {
        SourceSpan {
            file: path.to_string_lossy().into_owned(),
            start: SourcePoint {
                line: 1,
                column: 1,
                offset: 0,
            },
            end: SourcePoint {
                line: 1,
                column: 1,
                offset: 0,
            },
        }
    }

    fn touch(root: &Path, rel: &str) -> PathBuf {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, b"").unwrap();
        path.canonicalize().unwrap()
    }

    fn synthetic_backend_selection() -> SourceSelection {
        SourceSelection {
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
        }
    }

    #[test]
    fn build_variant_tag_map_returns_empty_for_none() {
        let tmp = tempfile::tempdir().unwrap();
        let map = build_variant_tag_map(None, tmp.path());
        assert!(map.is_empty());
    }

    #[test]
    fn build_variant_tag_map_returns_empty_for_empty_ir() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = BuildConfigIR {
            is_empty: true,
            ..Default::default()
        };
        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        assert!(map.is_empty());
    }

    #[test]
    fn build_variant_tag_map_keys_each_variant_file() {
        let tmp = tempfile::tempdir().unwrap();
        let alpha = touch(tmp.path(), "src/backend_alpha.c");
        let beta = touch(tmp.path(), "src/backend_beta.c");

        let cfg = BuildConfigIR {
            variables: vec![ConfigVariable {
                name: "BACKEND".into(),
                kind: build_config::ConfigVarKind::Enum {
                    values: vec!["alpha".into(), "beta".into()],
                    numeric: false,
                },
                default: Some("alpha".into()),
            }],
            source_selections: vec![synthetic_backend_selection()],
            ..Default::default()
        };

        let map = build_variant_tag_map(Some(&cfg), tmp.path());

        assert_eq!(
            map.get(&alpha),
            Some(&vec![("BACKEND".to_string(), "alpha".to_string())])
        );
        assert_eq!(
            map.get(&beta),
            Some(&vec![("BACKEND".to_string(), "beta".to_string())])
        );
    }

    #[test]
    fn variant_tags_for_returns_empty_when_map_empty() {
        let map = VariantTagMap::new();
        let span = span_for(Path::new("/does/not/matter.c"));
        assert!(variant_tags_for(&span, &map).is_empty());
    }

    #[test]
    fn variant_tags_for_returns_empty_for_unselected_file() {
        let tmp = tempfile::tempdir().unwrap();
        let alpha = touch(tmp.path(), "src/backend_alpha.c");
        let main = touch(tmp.path(), "src/main.c");

        let cfg = BuildConfigIR {
            source_selections: vec![synthetic_backend_selection()],
            ..Default::default()
        };
        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        // Sanity: alpha is in the map, main isn't.
        assert!(map.contains_key(&alpha));
        assert!(variant_tags_for(&span_for(&main), &map).is_empty());
    }

    #[test]
    fn variant_tags_for_matches_alpha_file() {
        let tmp = tempfile::tempdir().unwrap();
        let alpha = touch(tmp.path(), "src/backend_alpha.c");

        let cfg = BuildConfigIR {
            source_selections: vec![synthetic_backend_selection()],
            ..Default::default()
        };
        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        assert_eq!(
            variant_tags_for(&span_for(&alpha), &map),
            vec![("BACKEND".into(), "alpha".into())]
        );
    }

    #[test]
    fn build_variant_tag_map_accumulates_two_selections_for_same_file() {
        // A file referenced by two distinct SourceSelections (e.g. the same
        // file picked under both BACKEND=alpha and a hypothetical
        // FLAVOR=plain) should receive both tags.
        let tmp = tempfile::tempdir().unwrap();
        let shared = touch(tmp.path(), "src/shared.c");

        let cfg = BuildConfigIR {
            source_selections: vec![
                SourceSelection {
                    target: "first".into(),
                    driving_var: "BACKEND".into(),
                    variants: vec![SourceVariant {
                        value: "alpha".into(),
                        files: vec![PathBuf::from("src/shared.c")],
                    }],
                },
                SourceSelection {
                    target: "second".into(),
                    driving_var: "FLAVOR".into(),
                    variants: vec![SourceVariant {
                        value: "plain".into(),
                        files: vec![PathBuf::from("src/shared.c")],
                    }],
                },
            ],
            ..Default::default()
        };
        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        let tags = map.get(&shared).cloned().unwrap_or_default();
        assert!(tags.contains(&("BACKEND".into(), "alpha".into())));
        assert!(tags.contains(&("FLAVOR".into(), "plain".into())));
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn subdir_selection_inner_source_selection_stacks_outer_and_inner_tags() {
        // Sphincs-shape: top-level `add_subdirectory(${HASH_BACKEND})` with a
        // `blake` variant whose own CMakeLists has its own `${BACKEND}`-driven
        // source selection. A file picked by the inner selection gets BOTH
        // the outer `HASH_BACKEND=blake` and the inner `BACKEND=alpha` tag.
        use build_config::{SubdirSelection, SubdirVariant};
        let tmp = tempfile::tempdir().unwrap();
        let inner_file = touch(tmp.path(), "lib/blake/src/backend_alpha.c");

        let cfg = BuildConfigIR {
            subdir_selections: vec![SubdirSelection {
                driving_var: "HASH_BACKEND".into(),
                variants: vec![SubdirVariant {
                    value: "blake".into(),
                    path: PathBuf::from("lib/blake"),
                    source_selections: vec![SourceSelection {
                        target: "blake_core".into(),
                        driving_var: "BACKEND".into(),
                        variants: vec![SourceVariant {
                            value: "alpha".into(),
                            files: vec![PathBuf::from("lib/blake/src/backend_alpha.c")],
                        }],
                    }],
                    defines: Vec::new(),
                    conditional_targets: Vec::new(),
                    subdir_selections: Vec::new(),
                    targets: Vec::new(),
                }],
            }],
            ..Default::default()
        };

        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        let tags = map.get(&inner_file).cloned().unwrap_or_default();
        assert_eq!(tags.len(), 2, "tags = {tags:?}");
        assert_eq!(tags[0], ("HASH_BACKEND".into(), "blake".into()));
        assert_eq!(tags[1], ("BACKEND".into(), "alpha".into()));
    }

    #[test]
    fn subdir_variant_plain_target_files_get_outer_tag_only() {
        // Files declared by a plain `add_executable` / `add_library` inside a
        // subdir variant (carried by `SubdirVariant.targets`) are exclusive
        // to that variant and so must receive the outer driving-var tag.
        use build_config::{SubdirSelection, SubdirVariant, TargetDecl, TargetKind};
        let tmp = tempfile::tempdir().unwrap();
        let plain = touch(tmp.path(), "lib/blake/src/utils.c");

        let cfg = BuildConfigIR {
            subdir_selections: vec![SubdirSelection {
                driving_var: "HASH_BACKEND".into(),
                variants: vec![SubdirVariant {
                    value: "blake".into(),
                    path: PathBuf::from("lib/blake"),
                    targets: vec![TargetDecl {
                        name: "blake_core".into(),
                        kind: TargetKind::Library,
                        files: vec![PathBuf::from("lib/blake/src/utils.c")],
                    }],
                    defines: Vec::new(),
                    source_selections: Vec::new(),
                    conditional_targets: Vec::new(),
                    subdir_selections: Vec::new(),
                }],
            }],
            ..Default::default()
        };

        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        let tags = map.get(&plain).cloned().unwrap_or_default();
        assert_eq!(
            tags,
            vec![("HASH_BACKEND".into(), "blake".into())],
            "plain-target file should get only the outer subdir tag",
        );
    }

    #[test]
    fn nested_subdir_selections_accumulate_outer_tags() {
        // `add_subdirectory(${OUTER})` -> outer variant has its own
        // `add_subdirectory(${INNER})`. A file in the deepest variant carries
        // BOTH outer tags.
        use build_config::{SubdirSelection, SubdirVariant, TargetDecl, TargetKind};
        let tmp = tempfile::tempdir().unwrap();
        let deep = touch(tmp.path(), "lib/a/b/src/leaf.c");

        let inner = SubdirSelection {
            driving_var: "INNER".into(),
            variants: vec![SubdirVariant {
                value: "b".into(),
                path: PathBuf::from("lib/a/b"),
                targets: vec![TargetDecl {
                    name: "leaf".into(),
                    kind: TargetKind::Library,
                    files: vec![PathBuf::from("lib/a/b/src/leaf.c")],
                }],
                defines: Vec::new(),
                source_selections: Vec::new(),
                conditional_targets: Vec::new(),
                subdir_selections: Vec::new(),
            }],
        };
        let cfg = BuildConfigIR {
            subdir_selections: vec![SubdirSelection {
                driving_var: "OUTER".into(),
                variants: vec![SubdirVariant {
                    value: "a".into(),
                    path: PathBuf::from("lib/a"),
                    subdir_selections: vec![inner],
                    defines: Vec::new(),
                    source_selections: Vec::new(),
                    conditional_targets: Vec::new(),
                    targets: Vec::new(),
                }],
            }],
            ..Default::default()
        };

        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        let tags = map.get(&deep).cloned().unwrap_or_default();
        assert_eq!(tags.len(), 2, "tags = {tags:?}");
        assert_eq!(tags[0], ("OUTER".into(), "a".into()));
        assert_eq!(tags[1], ("INNER".into(), "b".into()));
    }

    #[test]
    fn flat_ir_unchanged_by_subdir_walker() {
        // Anti-regression: a flat IR (no subdir_selections) must produce the
        // same map as the pre-recursive implementation. We assert this by
        // checking that a file referenced only by a top-level source selection
        // gets exactly one tag (its own selection's variant), nothing extra.
        let tmp = tempfile::tempdir().unwrap();
        let alpha = touch(tmp.path(), "src/backend_alpha.c");
        let cfg = BuildConfigIR {
            source_selections: vec![synthetic_backend_selection()],
            ..Default::default()
        };
        let map = build_variant_tag_map(Some(&cfg), tmp.path());
        assert_eq!(
            map.get(&alpha),
            Some(&vec![("BACKEND".into(), "alpha".into())]),
            "flat IR must produce a single tag per file",
        );
    }

    #[test]
    fn entity_with_no_variant_tags_serializes_without_field() {
        // Anti-regression: an entity with an empty variant_tags vec must not
        // emit a `variant_tags` key in JSON, so existing TRACTOR-corpus
        // outputs remain byte-equal.
        let entity = TopLevelEntity {
            kind: EntityKind::FunctionDecl,
            source_text: "void f(void) {}".to_string(),
            span: span_for(Path::new("/x/y/z.c")),
            ast: None,
            annotations: EntityAnnotations::default(),
            sub_entities: Vec::new(),
            variant_tags: Vec::new(),
        };
        let json = serde_json::to_string(&entity).unwrap();
        assert!(
            !json.contains("variant_tags"),
            "empty variant_tags must be skipped in JSON, got: {json}"
        );
    }

    #[test]
    fn entity_with_variant_tags_round_trips_through_json() {
        let entity = TopLevelEntity {
            kind: EntityKind::FunctionDecl,
            source_text: "void f(void) {}".to_string(),
            span: span_for(Path::new("/x/y/backend_alpha.c")),
            ast: None,
            annotations: EntityAnnotations::default(),
            sub_entities: Vec::new(),
            variant_tags: vec![("BACKEND".into(), "alpha".into())],
        };
        let json = serde_json::to_string(&entity).unwrap();
        assert!(json.contains("variant_tags"));
        let round_trip: TopLevelEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(round_trip.variant_tags, entity.variant_tags);
    }

    #[test]
    fn entity_json_without_variant_tags_deserializes_with_empty_default() {
        // Existing JSON from `main` does not contain `variant_tags`. Make sure
        // deserialization defaults it to the empty vec.
        let legacy = r#"{
            "kind": "FunctionDecl",
            "source_text": "void f(void) {}",
            "span": {
                "file": "/x/y/z.c",
                "start": {"line": 1, "column": 1, "offset": 0},
                "end": {"line": 1, "column": 1, "offset": 0}
            },
            "ast": null
        }"#;
        let parsed: TopLevelEntity = serde_json::from_str(legacy).unwrap();
        assert!(parsed.variant_tags.is_empty());
    }
}
