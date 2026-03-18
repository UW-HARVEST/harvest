mod ast;

use clang::{Clang as LibClang, EntityKind, EntityVisitResult, Index};
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};
use tracing::{debug, info, warn};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Clang {
    TypedefDecl {
        name: String,
    },
    FunctionDecl {
        name: String,
        #[serde(rename = "storageClass")]
        storage_class: Option<String>,
        params: Vec<Option<String>>,
    },
    RecordDecl {
        name: Option<String>,
        #[serde(rename = "tagUsed")]
        tag_used: Option<String>,
    },
    EnumDecl {
        name: Option<String>,
    },
    VarDecl {
        name: String,
        #[serde(rename = "storageClass")]
        storage_class: Option<String>,
    },
    MacroDefinition,
    IncludeDirective,
    ConditionalDirective,
    Other {
        kind: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SourcePoint {
    pub line: u32,
    pub column: u32,
    pub offset: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SourceSpan {
    pub file: String,
    pub start: SourcePoint,
    pub end: SourcePoint,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TopLevelItem {
    pub kind: TopLevelKind,
    pub source_text: String,
    pub span: SourceSpan,
    pub ast: Option<Clang>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ArgOrigin {
    pub span: SourceSpan,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ArgWithOrigin {
    pub value: String,
    pub origin: ArgOrigin,
}

#[derive(Serialize)]
pub struct ClangAst {
    pub items: Vec<TopLevelItem>,
    pub include_paths: Vec<ArgWithOrigin>,
    pub defines: Vec<ArgWithOrigin>,
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

        let src_dir = tempfile::TempDir::new()?;
        rs.dir.materialize(src_dir.path())?;

        let mut file_bytes: HashMap<String, Vec<u8>> = HashMap::new();
        let mut source_files: Vec<String> = Vec::new();

        for (rel_path, bytes) in rs.dir.files_recursive() {
            if ast::is_c_or_header(&rel_path) {
                let rel = ast::normalize_rel_path(&rel_path);
                source_files.push(rel.clone());
                file_bytes.insert(rel, bytes.to_vec());
            }
        }

        source_files.sort();
        info!("Parsing {} source files", source_files.len());

        let clang = LibClang::new().map_err(|e| format!("Failed to initialize libclang: {e}"))?;
        let index = Index::new(&clang, false, false);

        let include_paths: Vec<ArgWithOrigin> = Vec::new();
        let defines: Vec<ArgWithOrigin> = Vec::new();
        let compiler_args: Vec<ArgWithOrigin> = Vec::new();

        let mut common_parse_args: Vec<String> = vec!["-std=gnu11".to_string()];
        common_parse_args.push(format!("-I{}", src_dir.path().to_string_lossy()));

        let mut items = Vec::new();

        for rel_file in &source_files {
            let abs_file = src_dir.path().join(rel_file);
            let mut parser_arg_values = common_parse_args.clone();
            parser_arg_values.extend(
                ast::language_args_for_file(rel_file)
                    .iter()
                    .map(|s| s.to_string()),
            );

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

            for child in root.get_children() {
                let Some(decl_kind) = ast::map_top_level_decl_kind(child.get_kind()) else {
                    continue;
                };
                if !child.is_in_main_file() {
                    continue;
                }

                if let Some(item) =
                    ast::decl_item_from_entity(decl_kind, &child, src_dir.path(), &file_bytes)
                {
                    items.push(item);
                }
            }

            root.visit_children(|entity, _| {
                if !entity.is_preprocessing() || !entity.is_in_main_file() {
                    return EntityVisitResult::Recurse;
                }

                let kind = match entity.get_kind() {
                    EntityKind::MacroDefinition => Some("MacroDefinition"),
                    EntityKind::InclusionDirective => Some("IncludeDirective"),
                    EntityKind::PreprocessingDirective => Some("PreprocessingDirective"),
                    _ => None,
                };

                let Some(kind) = kind else {
                    return EntityVisitResult::Recurse;
                };

                let top_kind = match kind {
                    "MacroDefinition" => TopLevelKind::MacroDefinition,
                    "IncludeDirective" => TopLevelKind::IncludeDirective,
                    _ => TopLevelKind::ConditionalDirective,
                };

                if let Some(item) = ast::preprocessor_item_from_entity(
                    top_kind,
                    &entity,
                    src_dir.path(),
                    &file_bytes,
                ) {
                    items.push(item);
                }

                EntityVisitResult::Recurse
            });

            info!("Parsed {}", rel_file);
        }

        let _ = id;

        Ok(Box::new(ClangAst {
            items,
            include_paths,
            defines,
            compiler_args,
        }))
    }
}
