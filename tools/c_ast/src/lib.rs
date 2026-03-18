mod ast;
mod rsm;

use clang::{Clang as LibClang, EntityKind as ClangEntityKind, EntityVisitResult, Index};
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use std::{collections::HashMap, path::Path};
use tracing::{debug, info, warn};

pub use ast::ClangAST;
pub use rsm::{
    EntityKind, PreprocessorDirective, RichSourceMap, SourcePoint, SourceSpan, TopLevelEntity,
};

pub struct ParseToAst;

fn process_file(
    index: &Index,
    src_root: &Path,
    rel_file: &str,
    common_parse_args: &[String],
    file_bytes: &HashMap<String, Vec<u8>>,
    out: &mut RichSourceMap,
) {
    let abs_file = src_root.join(rel_file);
    let mut parser_arg_values = common_parse_args.to_vec();
    parser_arg_values.extend(
        ast::language_args_for_file(rel_file)
            .iter()
            .map(|s| s.to_string()),
    );

    debug!("Parsing {} with args: {:?}", rel_file, parser_arg_values);

    let mut parser = index.parser(&abs_file);
    parser
        .arguments(&parser_arg_values)
        .detailed_preprocessing_record(true);

    let tu = match parser.parse() {
        Ok(tu) => tu,
        Err(e) => {
            warn!("Skipping {} due to parse failure: {:?}", rel_file, e);
            return;
        }
    };

    let root = tu.get_entity();

    for child in root.get_children() {
        let Some(decl_kind) = ast::map_top_level_decl_kind(child.get_kind()) else {
            continue;
        };
        if !child.is_in_main_file() {
            continue;
        }

        if let Some(item) = ast::decl_item_from_entity(decl_kind, &child, src_root, file_bytes) {
            out.push_sorted(item);
        }
    }

    root.visit_children(|entity, _| {
        if !entity.is_preprocessing() || !entity.is_in_main_file() {
            return EntityVisitResult::Recurse;
        }

        let kind = match entity.get_kind() {
            ClangEntityKind::MacroDefinition => Some("MacroDefinition"),
            ClangEntityKind::InclusionDirective => Some("IncludeDirective"),
            ClangEntityKind::PreprocessingDirective => Some("PreprocessingDirective"),
            _ => None,
        };

        let Some(kind) = kind else {
            return EntityVisitResult::Recurse;
        };

        let top_kind = match kind {
            "MacroDefinition" => EntityKind::MacroDefinition,
            "IncludeDirective" => EntityKind::IncludeDirective,
            _ => EntityKind::ConditionalDirective,
        };

        if let Some(item) =
            ast::preprocessor_item_from_entity(top_kind, &entity, src_root, file_bytes)
        {
            out.push_sorted(item);
        }

        EntityVisitResult::Recurse
    });

    info!("Parsed {}", rel_file);
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
            if ast::is_c_or_header(&rel_path) {
                let rel = ast::normalize_rel_path(&rel_path);
                source_files.push(rel.clone());
                file_bytes.insert(rel, bytes.to_vec());
            }
        }

        source_files.sort();
        info!("Parsing {} source files", source_files.len());

        let clang = LibClang::new().map_err(|e| format!("Failed to initialize libclang: {e}"))?;
        let index = Index::new(&clang, false, false);

        let include_paths: Vec<PreprocessorDirective> = Vec::new();
        let defines: Vec<PreprocessorDirective> = Vec::new();
        let compiler_args: Vec<PreprocessorDirective> = Vec::new();

        let mut common_parse_args: Vec<String> = vec!["-std=gnu11".to_string()];
        common_parse_args.push(format!("-I{}", src_dir.path().to_string_lossy()));

        let mut out = RichSourceMap {
            app_types: Vec::new(),
            app_globals: Vec::new(),
            app_functions: Vec::new(),
            include_paths,
            defines,
            compiler_args,
        };

        for rel_file in &source_files {
            process_file(
                &index,
                src_dir.path(),
                rel_file,
                &common_parse_args,
                &file_bytes,
                &mut out,
            );
        }

        let _ = id;

        Ok(Box::new(out))
    }
}
