use harvest_core::Representation;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::ClangAST;

/// Representaiton of a single point in a source file, used for source mapping.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SourcePoint {
    pub line: u32,
    pub column: u32,
    pub offset: u32,
}

/// Our own simplified representation of a span.
/// Corresponds to clang's spelling_loc.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SourceSpan {
    pub file: String,
    pub start: SourcePoint,
    pub end: SourcePoint,
}

/// The complete set of clang entities (AST kinds + preprocessor directives) that we care about extracting from the source.
/// Contains all possible top-level declarations.
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

/// Coerces libClang's EntityKind into our own EntityKind enum, which is simplified for our use case.
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

/// A top-level entity extracted from the source code, along with its source span and original source text.
/// In short, this represents a "thing we need to translate"
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TopLevelEntity {
    pub kind: EntityKind,
    pub source_text: String,
    pub span: SourceSpan,
    pub ast: Option<ClangAST>,
}

/// This is the output of the parsing step, and therefore this tool.
/// It contains both the source text and the AST for each top-level entity, as well as preprocessor directives (include paths, defines, etc).
/// It is designed such that every all text in the source code has exactly one unique representation in the RichSourceMap, either as a top-level entity or as a preprocessor directive.
/// These source-level entities may then have a corresponding AST representation, depending on whether they persist through preprocessing.
#[derive(Serialize)]
pub struct RichSourceMap {
    pub app_types: Vec<TopLevelEntity>,
    pub app_globals: Vec<TopLevelEntity>,
    pub app_functions: Vec<TopLevelEntity>,
    pub app_func_sigs: Vec<TopLevelEntity>,
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
            app_func_sigs: Vec::new(),
            include_paths: Vec::new(),
            defines: Vec::new(),
            compiler_args: Vec::new(),
        }
    }

    /// Sort and store entities in the RichSourceMap based on their kind.
    pub(crate) fn push_entity(&mut self, item: TopLevelEntity, child: &clang::Entity<'_>) {
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
                if child.is_definition() {
                    self.app_functions.push(item);
                } else {
                    self.app_func_sigs.push(item);
                }
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

    /// Iterate over the entities that survive preprocessing.
    /// They should all have AST representations.
    pub fn iter_definitions(&self) -> impl Iterator<Item = &TopLevelEntity> {
        self.app_types
            .iter()
            .chain(self.app_globals.iter())
            .chain(self.app_functions.iter())
    }

    /// Iterate over all preprocessor directives (includes, defines, compiler args).
    pub fn iter_directives(&self) -> impl Iterator<Item = &TopLevelEntity> {
        self.include_paths
            .iter()
            .chain(self.defines.iter())
            .chain(self.compiler_args.iter())
    }
}

impl std::fmt::Display for RichSourceMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total = self.app_types.len()
            + self.app_globals.len()
            + self.app_functions.len()
            + self.app_func_sigs.len();
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

impl Default for RichSourceMap {
    fn default() -> Self {
        Self::new()
    }
}
