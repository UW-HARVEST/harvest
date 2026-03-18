use clang::{EntityKind, source::SourceRange};
use std::{collections::HashMap, path::Path};

use crate::{Clang, SourcePoint, SourceSpan, TopLevelItem, TopLevelKind};

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

pub(crate) fn map_top_level_decl_kind(kind: EntityKind) -> Option<DeclKind> {
    match kind {
        EntityKind::TypedefDecl => Some(DeclKind::Typedef),
        EntityKind::FunctionDecl => Some(DeclKind::Function),
        EntityKind::StructDecl | EntityKind::UnionDecl => Some(DeclKind::Record),
        EntityKind::EnumDecl => Some(DeclKind::Enum),
        EntityKind::VarDecl => Some(DeclKind::Var),
        _ => None,
    }
}

pub(crate) fn decl_item_from_entity(
    decl_kind: DeclKind,
    entity: &clang::Entity<'_>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<TopLevelItem> {
    let (span, source_text) = range_to_span_and_text(entity.get_range(), root_dir, file_bytes)?;

    let ast = match decl_kind {
        DeclKind::Typedef => Clang::TypedefDecl {
            name: entity.get_name().unwrap_or_default(),
        },
        DeclKind::Function => Clang::FunctionDecl {
            name: entity.get_name().unwrap_or_default(),
            storage_class: None,
            params: entity
                .get_children()
                .into_iter()
                .filter(|c| c.get_kind() == EntityKind::ParmDecl)
                .map(|p| p.get_name())
                .collect(),
        },
        DeclKind::Record => Clang::RecordDecl {
            name: entity.get_name(),
            tag_used: None,
        },
        DeclKind::Enum => Clang::EnumDecl {
            name: entity.get_name(),
        },
        DeclKind::Var => Clang::VarDecl {
            name: entity.get_name().unwrap_or_default(),
            storage_class: None,
        },
    };

    Some(TopLevelItem {
        kind: map_decl_to_top_level_kind(decl_kind),
        source_text,
        span,
        ast: Some(ast),
    })
}

fn map_decl_to_top_level_kind(kind: DeclKind) -> TopLevelKind {
    match kind {
        DeclKind::Typedef => TopLevelKind::TypedefDecl,
        DeclKind::Function => TopLevelKind::FunctionDecl,
        DeclKind::Record => TopLevelKind::RecordDecl,
        DeclKind::Enum => TopLevelKind::EnumDecl,
        DeclKind::Var => TopLevelKind::VarDecl,
    }
}

pub(crate) fn preprocessor_item_from_entity(
    kind: TopLevelKind,
    entity: &clang::Entity<'_>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<TopLevelItem> {
    let (span, source_text) = range_to_span_and_text(entity.get_range(), root_dir, file_bytes)?;

    if (kind == TopLevelKind::MacroDefinition || kind == TopLevelKind::IncludeDirective)
        && source_text.trim().is_empty()
    {
        return None;
    }

    if kind == TopLevelKind::ConditionalDirective && !is_conditional_directive(&source_text) {
        return None;
    }

    let ast = match kind {
        TopLevelKind::MacroDefinition => Some(Clang::MacroDefinition),
        TopLevelKind::IncludeDirective => Some(Clang::IncludeDirective),
        TopLevelKind::ConditionalDirective => Some(Clang::ConditionalDirective),
        _ => None,
    };

    Some(TopLevelItem {
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
