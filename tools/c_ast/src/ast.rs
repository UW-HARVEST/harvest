use serde::{Deserialize, Serialize};

use crate::EntityKind;

/// Persistent AST representation for the C source code.
/// We will extend this as we find that we need more info (from libClang).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClangAST {
    TypedefDecl {
        name: String,
    },
    FunctionDecl {
        name: String,
        #[serde(rename = "storageClass")]
        storage_class: Option<String>,
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

/// Extract info from a libClang Entity and convert it into our ClangAST representation.
/// Returns None for entities that are removed during preprocessing.
pub(crate) fn ast_from_entity(
    decl_kind: EntityKind,
    entity: &clang::Entity<'_>,
) -> Option<ClangAST> {
    match decl_kind {
        EntityKind::TypedefDecl => Some(ClangAST::TypedefDecl {
            name: entity.get_name().unwrap_or_default(),
        }),
        EntityKind::FunctionDecl => Some(ClangAST::FunctionDecl {
            name: entity.get_name().unwrap_or_default(),
            storage_class: None,
        }),
        EntityKind::RecordDecl | EntityKind::UnionDecl => Some(ClangAST::RecordDecl {
            name: entity.get_name(),
            tag_used: None,
        }),
        EntityKind::EnumDecl => Some(ClangAST::EnumDecl {
            name: entity.get_name(),
        }),
        EntityKind::VarDecl => Some(ClangAST::VarDecl {
            name: entity.get_name().unwrap_or_default(),
            storage_class: None,
        }),
        EntityKind::PreprocessingDirective => None,
        EntityKind::MacroDefinition => None,
        EntityKind::InclusionDirective => None,
    }
}
