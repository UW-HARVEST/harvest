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
    UnionDecl,
    EnumDecl,
    VarDecl,
    PreprocessingDirective,
    MacroDefinition,
    InclusionDirective,
}

pub(crate) fn map_top_level_decl_kind(kind: clang::EntityKind) -> Option<EntityKind> {
    match kind {
        clang::EntityKind::TypedefDecl => Some(EntityKind::TypedefDecl),
        clang::EntityKind::FunctionDecl => Some(EntityKind::FunctionDecl),
        clang::EntityKind::StructDecl => Some(EntityKind::RecordDecl),
        clang::EntityKind::UnionDecl => Some(EntityKind::UnionDecl),
        clang::EntityKind::EnumDecl => Some(EntityKind::EnumDecl),
        clang::EntityKind::VarDecl => Some(EntityKind::VarDecl),
        clang::EntityKind::PreprocessingDirective => Some(EntityKind::PreprocessingDirective),
        clang::EntityKind::MacroDefinition => Some(EntityKind::MacroDefinition),
        clang::EntityKind::InclusionDirective => Some(EntityKind::InclusionDirective),
        _ => None,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TopLevelEntity {
    pub kind: EntityKind,
    pub source_text: String,
    pub span: SourceSpan,
    pub ast: Option<ClangAST>,
}

#[derive(Serialize)]
pub struct RichSourceMap {
    pub app_types: Vec<TopLevelEntity>,
    pub app_globals: Vec<TopLevelEntity>,
    pub app_functions: Vec<TopLevelEntity>,
    pub include_paths: Vec<TopLevelEntity>,
    pub defines: Vec<TopLevelEntity>,
    pub compiler_args: Vec<TopLevelEntity>,
}

impl RichSourceMap {
    pub fn new() -> Self {
        Self {
            app_types: Vec::new(),
            app_globals: Vec::new(),
            app_functions: Vec::new(),
            include_paths: Vec::new(),
            defines: Vec::new(),
            compiler_args: Vec::new(),
        }
    }

    pub(crate) fn push_entity(&mut self, item: TopLevelEntity) {
        match item.kind {
            EntityKind::TypedefDecl
            | EntityKind::RecordDecl
            | EntityKind::UnionDecl
            | EntityKind::EnumDecl => {
                self.app_types.push(item);
            }
            EntityKind::VarDecl => {
                self.app_globals.push(item);
            }
            EntityKind::FunctionDecl => {
                self.app_functions.push(item);
            }
            EntityKind::InclusionDirective => {
                self.include_paths.push(item);
            }
            EntityKind::MacroDefinition => {
                self.defines.push(item);
            }
            EntityKind::PreprocessingDirective => {
                self.compiler_args.push(item);
            }
        }
    }

    pub fn iter_definitions(&self) -> impl Iterator<Item = &TopLevelEntity> {
        self.app_types
            .iter()
            .chain(self.app_globals.iter())
            .chain(self.app_functions.iter())
    }

    pub fn iter_directives(&self) -> impl Iterator<Item = &TopLevelEntity> {
        self.include_paths
            .iter()
            .chain(self.defines.iter())
            .chain(self.compiler_args.iter())
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
