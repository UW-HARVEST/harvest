use clang::{EntityKind as ClangEntityKind, source::SourceRange};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

use crate::{EntityKind, SourcePoint, SourceSpan, TopLevelDefinition};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClangAST {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclKind {
    Typedef,
    Function,
    Record,
    Enum,
    Var,
}

pub(crate) fn is_c_or_header(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("c") || ext.eq_ignore_ascii_case("h"),
        None => false,
    }
}

pub(crate) fn normalize_rel_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn language_args_for_file(path: &str) -> [&'static str; 2] {
    if path.ends_with(".h") {
        ["-x", "c-header"]
    } else {
        ["-x", "c"]
    }
}

pub(crate) fn map_top_level_decl_kind(kind: ClangEntityKind) -> Option<DeclKind> {
    match kind {
        ClangEntityKind::TypedefDecl => Some(DeclKind::Typedef),
        ClangEntityKind::FunctionDecl => Some(DeclKind::Function),
        ClangEntityKind::StructDecl | ClangEntityKind::UnionDecl => Some(DeclKind::Record),
        ClangEntityKind::EnumDecl => Some(DeclKind::Enum),
        ClangEntityKind::VarDecl => Some(DeclKind::Var),
        _ => None,
    }
}

pub(crate) fn decl_item_from_entity(
    decl_kind: DeclKind,
    entity: &clang::Entity<'_>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<TopLevelDefinition> {
    let (span, source_text) = range_to_span_and_text(entity.get_range(), root_dir, file_bytes)?;

    let ast = match decl_kind {
        DeclKind::Typedef => ClangAST::TypedefDecl {
            name: entity.get_name().unwrap_or_default(),
        },
        DeclKind::Function => ClangAST::FunctionDecl {
            name: entity.get_name().unwrap_or_default(),
            storage_class: None,
            params: entity
                .get_children()
                .into_iter()
                .filter(|c| c.get_kind() == ClangEntityKind::ParmDecl)
                .map(|p| p.get_name())
                .collect(),
        },
        DeclKind::Record => ClangAST::RecordDecl {
            name: entity.get_name(),
            tag_used: None,
        },
        DeclKind::Enum => ClangAST::EnumDecl {
            name: entity.get_name(),
        },
        DeclKind::Var => ClangAST::VarDecl {
            name: entity.get_name().unwrap_or_default(),
            storage_class: None,
        },
    };

    Some(TopLevelDefinition {
        kind: map_decl_to_top_level_kind(decl_kind),
        source_text,
        span,
        ast: Some(ast),
    })
}

fn map_decl_to_top_level_kind(kind: DeclKind) -> EntityKind {
    match kind {
        DeclKind::Typedef => EntityKind::TypedefDecl,
        DeclKind::Function => EntityKind::FunctionDecl,
        DeclKind::Record => EntityKind::RecordDecl,
        DeclKind::Enum => EntityKind::EnumDecl,
        DeclKind::Var => EntityKind::VarDecl,
    }
}

pub(crate) fn preprocessor_item_from_entity(
    kind: EntityKind,
    entity: &clang::Entity<'_>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<TopLevelDefinition> {
    let (span, source_text) = range_to_span_and_text(entity.get_range(), root_dir, file_bytes)?;

    if (kind == EntityKind::MacroDefinition || kind == EntityKind::IncludeDirective)
        && source_text.trim().is_empty()
    {
        return None;
    }

    if kind == EntityKind::ConditionalDirective && !is_conditional_directive(&source_text) {
        return None;
    }

    let ast = match kind {
        EntityKind::MacroDefinition => Some(ClangAST::MacroDefinition),
        EntityKind::IncludeDirective => Some(ClangAST::IncludeDirective),
        EntityKind::ConditionalDirective => Some(ClangAST::ConditionalDirective),
        _ => None,
    };

    Some(TopLevelDefinition {
        kind,
        source_text,
        span,
        ast,
    })
}

pub(crate) fn range_to_span_and_text(
    range: Option<SourceRange<'_>>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<(SourceSpan, String)> {
    let range = range?;
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

pub(crate) fn is_conditional_directive(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("#if")
        || t.starts_with("#ifdef")
        || t.starts_with("#ifndef")
        || t.starts_with("#elif")
        || t.starts_with("#else")
        || t.starts_with("#endif")
}
