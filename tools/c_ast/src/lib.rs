mod ast;
mod rsm;
mod utils;

use clang::{Clang as LibClang, Index};
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use std::{collections::HashMap, path::Path};
use tracing::{debug, info, warn};

pub use ast::ClangAST;
pub use rsm::{EntityKind, RichSourceMap, SourcePoint, SourceSpan, TopLevelEntity};

pub struct ParseToAst;

fn generate_parse_args(src_root: &Path, rel_file: &str) -> Vec<String> {
    let mut parser_arg_values = vec!["-std=gnu11".to_string()];
    parser_arg_values.push(format!("-I{}", src_root.to_string_lossy()));
    parser_arg_values.extend(
        utils::language_args_for_file(rel_file)
            .iter()
            .map(|s| s.to_string()),
    );
    parser_arg_values
}

fn build_parser<'a>(index: &'a Index, src_root: &Path, rel_file: &str) -> clang::Parser<'a> {
    let abs_file = src_root.join(rel_file);
    let parser_arg_values = generate_parse_args(src_root, rel_file);

    debug!("Parsing {} with args: {:?}", rel_file, parser_arg_values);

    let mut parser = index.parser(abs_file);
    parser.arguments(&parser_arg_values);
    parser
}

fn process_file(
    parser: clang::Parser<'_>,
    src_root: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
    out: &mut RichSourceMap,
) {
    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => {
            warn!("Skipping due to parse failure: {:?}", e);
            return;
        }
    };

    let root = tu.get_entity();

    for child in root.get_children() {
        let Some(decl_kind) = rsm::map_top_level_decl_kind(child.get_kind()) else {
            continue;
        };
        // Ignore imports
        if !child.is_in_main_file() {
            continue;
        }
        // Ignore function declarations that aren't definitions (i.e., don't have bodies).
        if decl_kind == EntityKind::FunctionDecl && !child.is_definition() {
            continue;
        }

        // Read the source text from the file
        let Some((span, source_text)) =
            utils::range_to_span_and_text(child.get_range(), src_root, file_bytes)
        else {
            continue;
        };

        // Extract the AST for this entity
        let ast = ast::ast_from_entity(decl_kind, &child);
        out.push_entity(TopLevelEntity {
            kind: decl_kind,
            source_text,
            span,
            ast,
        });
    }
}

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
            if utils::is_c_or_header(&rel_path) {
                let rel = utils::normalize_rel_path(&rel_path);
                source_files.push(rel.clone());
                file_bytes.insert(rel, bytes.to_vec());
            }
        }

        source_files.sort();
        info!("Parsing {} source files", source_files.len());

        let clang = LibClang::new().map_err(|e| format!("Failed to initialize libclang: {e}"))?;
        let index = Index::new(&clang, false, false);

        let mut out = RichSourceMap::new();

        for rel_file in &source_files {
            let parser = build_parser(&index, src_dir.path(), rel_file);
            process_file(parser, src_dir.path(), &file_bytes, &mut out);
        }

        info!(
            "Generated RichSourceMap:\n{}",
            serde_json::to_string_pretty(&out)?
        );
        Ok(Box::new(out))
    }
}
