use harvest_core::Representation;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::ClangAST;

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
pub enum EntityKind {
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
pub struct TopLevelEntity {
    pub kind: EntityKind,
    pub source_text: String,
    pub span: SourceSpan,
    pub ast: Option<ClangAST>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PreprocessorDirective {
    pub value: String,
    pub span: SourceSpan,
}

#[derive(Serialize)]
pub struct RichSourceMap {
    pub app_types: Vec<TopLevelEntity>,
    pub app_globals: Vec<TopLevelEntity>,
    pub app_functions: Vec<TopLevelEntity>,
    pub include_paths: Vec<PreprocessorDirective>,
    pub defines: Vec<PreprocessorDirective>,
    pub compiler_args: Vec<PreprocessorDirective>,
}

impl RichSourceMap {
    pub(crate) fn push_sorted(&mut self, item: TopLevelEntity) {
        match item.kind {
            EntityKind::TypedefDecl | EntityKind::RecordDecl | EntityKind::EnumDecl => {
                self.app_types.push(item);
            }
            EntityKind::VarDecl => {
                self.app_globals.push(item);
            }
            EntityKind::FunctionDecl => {
                self.app_functions.push(item);
            }
            _ => {
                // non-app declarations are intentionally not retained
            }
        }
    }
}

impl std::fmt::Display for RichSourceMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total = self.app_types.len() + self.app_globals.len() + self.app_functions.len();
        write!(f, "C top-level items ({} entries)", total)
    }
}

impl Representation for RichSourceMap {
    fn name(&self) -> &'static str {
        "clang_ast"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer(file, self).map_err(Into::into)
    }
}
