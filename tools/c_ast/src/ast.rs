use clang::EntityKind as ClangEntityKind;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

use crate::{EntityKind, TopLevelEntity, utils::range_to_span_and_text};

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
) -> Option<TopLevelEntity> {
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

    Some(TopLevelEntity {
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
