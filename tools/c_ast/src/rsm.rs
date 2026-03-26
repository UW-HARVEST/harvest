use harvest_core::Representation;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::ClangAST;
use crate::EntityAnnotations;

/// Representaiton of a single point in a source file, used for source mapping.
/// `column` and `offset` are UTF8 byte offsets, to match Clang's source location representation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SourcePoint {
    pub line: u32,
    pub column: u32,
    pub offset: u32,
}

/// Our own simplified representation of a span.
/// Corresponds to clang's spelling_loc.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
    #[serde(default)]
    pub annotations: EntityAnnotations,
    /// If this entity encloses another (sourcespan of child is fully contained within parent), we store it here.
    // This helps us deduplicate typedefs and struct/enum/union declarations.
    #[serde(default)]
    pub sub_entities: Vec<TopLevelEntity>,
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
                self.push_type(item);
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

    fn push_type(&mut self, item: TopLevelEntity) {
        Self::insert_type_into(&mut self.app_types, item);
    }

    fn insert_type_into(nodes: &mut Vec<TopLevelEntity>, mut item: TopLevelEntity) {
        // If an existing top-level node contains this item, attach it directly
        // as an immediate child. We do not preserve deeper sub-entity ordering.
        for node in nodes.iter_mut() {
            if Self::span_contains(&node.span, &item.span) {
                node.sub_entities.push(item);
                return;
            }
        }

        // Otherwise, absorb any existing nodes contained by this item.
        let mut i = 0;
        while i < nodes.len() {
            if Self::span_contains(&item.span, &nodes[i].span) {
                let child = nodes.remove(i);
                item.sub_entities.push(child);
            } else {
                i += 1;
            }
        }

        nodes.push(item);
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

    fn span_contains(parent: &SourceSpan, child: &SourceSpan) -> bool {
        if parent.file != child.file {
            return false;
        }

        parent.start.offset <= child.start.offset && parent.end.offset >= child.end.offset
    }

    /// Iterate over every top-level source entity tracked by the map.
    fn iter_all_entities(&self) -> impl Iterator<Item = &TopLevelEntity> {
        self.app_types
            .iter()
            .chain(self.app_globals.iter())
            .chain(self.app_functions.iter())
            .chain(self.app_func_sigs.iter())
            .chain(self.include_paths.iter())
            .chain(self.defines.iter())
            .chain(self.compiler_args.iter())
    }

    /// Returns true when the map satisfies its span invariants:
    /// - every span is internally valid (`start.offset <= end.offset`)
    /// - no two top-level entities overlap within the same file
    pub fn well_formed(&self) -> bool {
        let mut spans: Vec<&SourceSpan> = self
            .iter_all_entities()
            .map(|entity| &entity.span)
            .collect();

        spans.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.start.offset.cmp(&b.start.offset))
                .then(a.end.offset.cmp(&b.end.offset))
        });

        let mut prev_span: Option<&SourceSpan> = None;
        for span in spans {
            if span.start.offset > span.end.offset {
                return false;
            }

            if let Some(prev) = prev_span
                && prev.file == span.file
                && prev.end.offset > span.start.offset
            {
                return false;
            }

            prev_span = Some(span);
        }

        true
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
