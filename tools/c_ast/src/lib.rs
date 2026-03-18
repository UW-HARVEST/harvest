use clang::{Clang, EntityKind, EntityVisitResult, Index, source::SourceRange};
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use serde::Serialize;
use std::{collections::HashMap, path::Path};
use tracing::{debug, info, warn};

#[derive(Serialize, Debug, Clone)]
pub struct SourcePoint {
    pub line: u32,
    pub column: u32,
    pub offset: u32,
}

#[derive(Serialize, Debug, Clone)]
pub struct SourceSpan {
    /// Path relative to the root of the `RawSource` directory.
    pub file: String,
    /// Inclusive start position.
    pub start: SourcePoint,
    /// Exclusive end position.
    pub end: SourcePoint,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TopLevelKind {
    TypedefDecl,
    FunctionDecl,
    RecordDecl,
    EnumDecl,
    VarDecl,
    MacroDefinition,
    IncludeDirective,
    ConditionalDirective,
}

#[derive(Serialize, Debug, Clone)]
pub struct TopLevelItem {
    pub kind: TopLevelKind,
    /// Exact source text slice from the original file bytes.
    pub source_text: String,
    pub span: SourceSpan,
}

#[derive(Serialize, Debug, Clone)]
pub struct ArgOrigin {
    pub span: SourceSpan,
}

#[derive(Serialize, Debug, Clone)]
pub struct ArgWithOrigin {
    pub value: String,
    pub origin: ArgOrigin,
}

/// Flat extraction output from libclang for all `.c` and `.h` files in the input `RawSource`.
#[derive(Serialize)]
pub struct ClangAst {
    pub items: Vec<TopLevelItem>,
    /// Include paths passed to clang parser invocations.
    pub include_paths: Vec<ArgWithOrigin>,
    /// Command-line macro definitions passed as `-D...`.
    pub defines: Vec<ArgWithOrigin>,
    /// Common compiler flags passed to all parser invocations.
    pub compiler_args: Vec<ArgWithOrigin>,
}

impl std::fmt::Display for ClangAst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "C top-level items ({} entries)", self.items.len())
    }
}

impl Representation for ClangAst {
    fn name(&self) -> &'static str {
        "clang_ast"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer(file, self).map_err(Into::into)
    }
}

pub struct ParseToAst;

impl Tool for ParseToAst {
    fn name(&self) -> &'static str {
        "parse_to_ast"
    }

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

        // We parse each source independently, including headers, to guarantee
        // extraction even when a header is never included by a `.c` file.
        let src_dir = tempfile::TempDir::new()?;
        rs.dir.materialize(src_dir.path())?;

        let mut file_bytes: HashMap<String, Vec<u8>> = HashMap::new();
        let mut source_files: Vec<String> = Vec::new();

        for (rel_path, bytes) in rs.dir.files_recursive() {
            if is_c_or_header(&rel_path) {
                let rel = normalize_rel_path(&rel_path);
                source_files.push(rel.clone());
                file_bytes.insert(rel, bytes.to_vec());
            }
        }

        source_files.sort();

        info!("Parsing {} source files", source_files.len());

        let clang = Clang::new().map_err(|e| format!("Failed to initialize libclang: {e}"))?;
        let index = Index::new(&clang, false, false);

        // Source-derived argument origins only.
        // Tool-default args are still used for parsing, but are not emitted.
        let include_paths: Vec<ArgWithOrigin> = Vec::new();
        let defines: Vec<ArgWithOrigin> = Vec::new();
        let compiler_args: Vec<ArgWithOrigin> = Vec::new();

        let mut common_parse_args: Vec<String> = vec!["-std=gnu11".to_string()];
        common_parse_args.push(format!("-I{}", src_dir.path().to_string_lossy()));

        let mut items = Vec::new();
        for rel_file in &source_files {
            let abs_file = src_dir.path().join(rel_file);
            let lang_args = language_args_for_file(rel_file);
            let mut parser_arg_values = common_parse_args.clone();
            parser_arg_values.extend(lang_args.iter().map(|s| s.to_string()));

            debug!("Parsing {} with args: {:?}", rel_file, parser_arg_values);

            let mut parser = index.parser(&abs_file);
            parser
                .arguments(&parser_arg_values)
                .detailed_preprocessing_record(true);

            let tu = match parser.parse() {
                Ok(tu) => tu,
                Err(e) => {
                    warn!("Skipping {} due to parse failure: {:?}", rel_file, e);
                    continue;
                }
            };

            let root = tu.get_entity();

            // Top-level declarations: direct children of the translation unit.
            for child in root.get_children() {
                let Some(kind) = map_top_level_decl_kind(child.get_kind()) else {
                    continue;
                };
                if !child.is_in_main_file() {
                    continue;
                }
                if let Some(item) =
                    entity_to_item(kind, child.get_range(), src_dir.path(), &file_bytes, None)
                {
                    items.push(item);
                }
            }

            // Preprocessor entities: walk recursively to collect macros/includes/conditionals.
            root.visit_children(|entity, _| {
                if !entity.is_preprocessing() || !entity.is_in_main_file() {
                    return EntityVisitResult::Recurse;
                }

                let entity_kind = entity.get_kind();
                let top_kind = match entity_kind {
                    EntityKind::MacroDefinition => Some(TopLevelKind::MacroDefinition),
                    EntityKind::InclusionDirective => Some(TopLevelKind::IncludeDirective),
                    EntityKind::PreprocessingDirective => Some(TopLevelKind::ConditionalDirective),
                    _ => None,
                };

                let Some(top_kind) = top_kind else {
                    return EntityVisitResult::Recurse;
                };

                let item = entity_to_item(
                    top_kind,
                    entity.get_range(),
                    src_dir.path(),
                    &file_bytes,
                    Some(entity_kind),
                );

                if let Some(item) = item {
                    // Keep only conditional directives for PreprocessingDirective.
                    if top_kind == TopLevelKind::ConditionalDirective
                        && !is_conditional_directive(&item.source_text)
                    {
                        return EntityVisitResult::Recurse;
                    }
                    items.push(item);
                }

                EntityVisitResult::Recurse
            });

            info!("Parsed {}", rel_file);
        }

        Ok(Box::new(ClangAst {
            items,
            include_paths,
            defines,
            compiler_args,
        }))
    }
}

fn is_c_or_header(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("c") || ext.eq_ignore_ascii_case("h"),
        None => false,
    }
}

fn normalize_rel_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn language_args_for_file(path: &str) -> [&'static str; 2] {
    if path.ends_with(".h") {
        ["-x", "c-header"]
    } else {
        ["-x", "c"]
    }
}

fn map_top_level_decl_kind(kind: EntityKind) -> Option<TopLevelKind> {
    match kind {
        EntityKind::TypedefDecl => Some(TopLevelKind::TypedefDecl),
        EntityKind::FunctionDecl => Some(TopLevelKind::FunctionDecl),
        EntityKind::StructDecl | EntityKind::UnionDecl => Some(TopLevelKind::RecordDecl),
        EntityKind::EnumDecl => Some(TopLevelKind::EnumDecl),
        EntityKind::VarDecl => Some(TopLevelKind::VarDecl),
        _ => None,
    }
}

fn entity_to_item(
    kind: TopLevelKind,
    range: Option<SourceRange<'_>>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
    source_kind: Option<EntityKind>,
) -> Option<TopLevelItem> {
    let range = range?;
    let (span, source_text) = range_to_span_and_text(range, root_dir, file_bytes)?;

    // For macro definitions and includes, keep empty-text items out.
    if matches!(
        source_kind,
        Some(EntityKind::MacroDefinition | EntityKind::InclusionDirective)
    ) && source_text.trim().is_empty()
    {
        return None;
    }

    Some(TopLevelItem {
        kind,
        source_text,
        span,
    })
}

fn range_to_span_and_text(
    range: SourceRange<'_>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<(SourceSpan, String)> {
    let start = range.get_start().get_file_location();
    let end = range.get_end().get_file_location();

    let start_file = start.file?;
    let end_file = end.file?;

    let start_path = start_file.get_path();
    let end_path = end_file.get_path();
    if start_path != end_path {
        return None;
    }

    let rel_path = start_path
        .strip_prefix(root_dir)
        .ok()
        .map(normalize_rel_path)
        .unwrap_or_else(|| start_path.to_string_lossy().replace('\\', "/"));

    let bytes = file_bytes.get(&rel_path)?;
    let start_offset = start.offset as usize;
    let end_offset = end.offset as usize;

    if start_offset > end_offset || end_offset > bytes.len() {
        return None;
    }

    let source_text = String::from_utf8_lossy(&bytes[start_offset..end_offset]).to_string();

    let span = SourceSpan {
        file: rel_path,
        start: SourcePoint {
            line: start.line,
            column: start.column,
            offset: start.offset,
        },
        end: SourcePoint {
            line: end.line,
            column: end.column,
            offset: end.offset,
        },
    };

    Some((span, source_text))
}

fn is_conditional_directive(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("#if")
        || t.starts_with("#ifdef")
        || t.starts_with("#ifndef")
        || t.starts_with("#elif")
        || t.starts_with("#else")
        || t.starts_with("#endif")
}
