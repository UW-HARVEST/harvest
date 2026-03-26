mod annotations;
mod ast;
mod rsm;
mod utils;

use clang::{Clang as LibClang, Index};
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use std::path::Path;
use tracing::{debug, warn};

pub use annotations::{EntityAnnotations, annotate_visibility};
pub use ast::ClangAST;
pub use rsm::{EntityKind, RichSourceMap, SourcePoint, SourceSpan, TopLevelEntity};

pub struct ParseToAst;

/// Utility function to generate libClang parser arguments based on the source root and file being parsed.
/// This includes standard flags, include paths, and language specification based on file extension.
fn generate_parse_args(src_root: &Path, rel_file: &Path) -> Vec<String> {
    let mut parser_arg_values = vec!["-std=gnu11".to_string()];
    parser_arg_values.push(format!("-I{}", src_root.to_string_lossy()));
    parser_arg_values.extend(
        utils::language_args_for_file(rel_file)
            .iter()
            .map(|s| s.to_string()),
    );
    parser_arg_values
}

/// Utility function to instantiate the libclang parser.
fn build_parser<'a>(index: &'a Index, src_root: &Path, rel_file: &Path) -> clang::Parser<'a> {
    let abs_file = src_root.join(rel_file);
    let parser_arg_values = generate_parse_args(src_root, rel_file);

    debug!(
        "Parsing {} with args: {:?}",
        rel_file.to_string_lossy(),
        parser_arg_values
    );

    let mut parser = index.parser(abs_file);
    parser.detailed_preprocessing_record(true);
    parser.arguments(&parser_arg_values);
    parser
}

/// Extract top-level entities from the file at `rel_path`.
/// This includes both entities that survive preprocessing (types, functions, globals) and preprocessor directives (includes, defines, compiler args).
fn extract_entities(parser: clang::Parser<'_>, rel_file: &Path, out: &mut RichSourceMap) {
    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => {
            warn!("Skipping due to parse failure: {:?}", e);
            return;
        }
    };

    let root = tu.get_entity();

    for child in root.get_children() {
        // Ignore entities that we don't care about.
        let Some(decl_kind) = rsm::map_top_level_decl_kind(child.get_kind()) else {
            continue;
        };

        // Ignore imports
        if child.is_in_system_header() || utils::get_file_location(&child).is_none() {
            continue;
        }

        // Read the source text from the file
        let Some((span, source_text)) = utils::get_span_and_text(&child, rel_file) else {
            continue;
        };

        // Extract the AST for this entity
        let ast = ast::ast_from_entity(decl_kind, &child);
        out.push_entity(
            TopLevelEntity {
                kind: decl_kind,
                source_text,
                span,
                ast,
                annotations: EntityAnnotations::default(),
                sub_entities: Vec::new(),
            },
            &child,
        );
    }
}

impl Tool for ParseToAst {
    fn name(&self) -> &'static str {
        "parse_to_ast"
    }

    /// For each C and header file in the RawSource, parse it with libClang and extract top-level declarations into a RichSourceMap.
    /// Captures preprocessor information such as include paths and defines as well.
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

        let clang = LibClang::new().map_err(|e| format!("Failed to initialize libclang: {e}"))?;
        let index = Index::new(&clang, false, false);

        let mut out = RichSourceMap::new();

        for (rel_path, _) in rs.dir.files_recursive() {
            if utils::should_skip_path(&rel_path) {
                continue;
            }
            if !utils::is_c_or_header(&rel_path) {
                continue;
            }
            tracing::info!("Parsing file: {}", rel_path.to_string_lossy());
            let parser = build_parser(&index, src_dir.path(), &rel_path);
            extract_entities(parser, &rel_path, &mut out);
        }

        debug!(
            "Generated RichSourceMap:\n{}",
            serde_json::to_string_pretty(&out)?
        );
        Ok(Box::new(out))
    }
}
