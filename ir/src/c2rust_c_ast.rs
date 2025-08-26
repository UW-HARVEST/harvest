use serde::{Deserialize, Serialize};
use std::{
    any::Any, fmt::Display, fs::File, path::{Path, PathBuf}, process::Command
};

use c2rust_transpile::c_ast::{ConversionContext, TypedAstContext};

use crate::{
    HarvestIR, Representation,
    raw_source::{RawDir, RawEntry},
};

#[derive(Debug)]
pub struct CAst {
    _ast: Vec<TypedAstContext>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
struct CompileCmd {
    /// The working directory of the compilation. All paths specified in the command
    /// or file fields must be either absolute or relative to this directory.
    pub directory: PathBuf,
    /// The main translation unit source processed by this compilation step. This is
    /// used by tools as the key into the compilation database. There can be multiple
    /// command objects for the same file, for example if the same source file is compiled
    /// with different configurations.
    pub file: PathBuf,
}

fn populate_from(base: &Path) -> Vec<TypedAstContext> {
    let v: Vec<CompileCmd> = serde_json::from_reader(std::io::BufReader::new(File::open(base.join("compile_commands.json")).unwrap())).unwrap();
    v.iter().map(|cc| {
	ConversionContext::new(&c2rust_ast_exporter::get_untyped_ast(&cc.file, base, &[], false).unwrap()).typed_context
    }).collect()
}

impl Display for CAst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Representation for CAst {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl CAst {
    pub fn run_stage<'a>(ir: HarvestIR) -> Option<CAst> {
        for repr in ir.representations.values() {
            if let Some(r) = repr.as_any().downcast_ref::<RawDir>() {
                return Self::populate_from(r);
            }
        }
        None
    }

    pub fn populate_from(src: &RawDir) -> Option<CAst> {
        fn reify(src: &RawDir, dir: &Path) -> std::io::Result<()> {
            for (name, entry) in src.0.iter() {
                match entry {
                    RawEntry::File(contents) => {
                        std::fs::write(dir.join(name), contents).unwrap();
                    }
                    RawEntry::Dir(subdir) => {
                        std::fs::create_dir(dir.join(name))?;
                        reify(subdir, &dir.join(name))?;
                    }
                }
            }
            Ok(())
        }

	// Copy source directory to the file system somewhere temporary
        let td = tempdir::TempDir::new("harvest").unwrap();
        reify(src, td.path()).ok()?;

	// Use cmake to generate a `compile_commands.json` file in a
	// separate build directory
        let cc_dir = tempdir::TempDir::new("harvest").unwrap();
        Command::new("cmake")
            .arg("-DCMAKE_EXPORT_COMPILE_COMMANDS=1")
            .arg("-S")
            .arg(td.path())
            .arg("-B")
            .arg(cc_dir.path())
            .output()
            .ok()?;
	Some(Self {
	    _ast: populate_from(cc_dir.path())
        })
    }

    pub fn tree_crawl(&self) {
        tree_crawl::read_root(&self._ast);
    }
}

mod tree_crawl {
    use c2rust_transpile::c_ast::*;
    use std::collections::HashSet;

    pub fn read_root(ctxt: &TypedAstContext) {
        sort_out_top_lvls(ctxt);
    }

    pub fn ok_to_discard_non_canonical_top_levels(ctxt: &TypedAstContext) {
        // Some extra code to make sure that we're not losing anything
        // by discarding non-canonical declarations
        let mut top_canons = HashSet::new();
        let mut non_canon_to_check = Vec::new();

        for decl_id in ctxt.c_decls_top.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();

            if let CDeclKind::NonCanonicalDecl { canonical_decl } = &decl.kind {
                non_canon_to_check.push(*canonical_decl);
            } else {
                top_canons.insert(*decl_id);
            }
        }

        for canon_declid in non_canon_to_check {
            assert!(
                top_canons.contains(&canon_declid),
                "Found a NonCanonicalDecl whose corresponding \
                     canonical declaration was not present in the \
                     list of top-level declarations"
            );
        }
    }

    pub fn sort_out_top_lvls(ctxt: &TypedAstContext) {
        ok_to_discard_non_canonical_top_levels(ctxt);

        // HYPOTHESIS: implicit typedefs are always the same
        //             across all files; they simply reflect
        //             built-in compiler features
        let mut implicit_typedefs: Vec<CDeclId> = Vec::new();

        // Q: Do we need to keep around the MacroObject or MacroFunction
        //      top-level declarations?  I think we'll need a more
        //      advanced macro-processing approach eventually anyway
        let mut top_macros: Vec<CDeclId> = Vec::new();

        // Top level buckets that we care about
        let mut top_funcs: Vec<CDeclId> = Vec::new();
        let mut top_typedefs: Vec<CDeclId> = Vec::new();
        let mut top_structs: Vec<CDeclId> = Vec::new();
        let mut top_unions: Vec<CDeclId> = Vec::new();
        let mut top_vars: Vec<CDeclId> = Vec::new();

        // iterate over the top-level declarations
        for decl_id in ctxt.c_decls_top.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();

            match &decl.kind {
                CDeclKind::Function {
                    is_implicit: true, ..
                } => {
                    panic!(
                        "Unexpected: {}\n  {:?}",
                        "C99 bans implicit function definitions", decl
                    );
                }
                // top-levels that we'll cache just in case, but
                // which probably don't need to be used
                CDeclKind::Typedef {
                    is_implicit: true, ..
                } => {
                    implicit_typedefs.push(*decl_id);
                }
                CDeclKind::MacroObject { .. } => top_macros.push(*decl_id),
                CDeclKind::MacroFunction { .. } => top_macros.push(*decl_id),
                // our earlier check has certified that it's ok to
                // ignore the non-canonical declarations without losing
                // track of any top-level declarations in the process
                CDeclKind::NonCanonicalDecl { .. } => {}
                // top-levels
                // top-levels worth processing
                CDeclKind::Typedef { .. } => top_typedefs.push(*decl_id),
                CDeclKind::Struct { .. } => top_structs.push(*decl_id),
                CDeclKind::Union { .. } => top_unions.push(*decl_id),
                CDeclKind::Variable { .. } => top_vars.push(*decl_id),
                CDeclKind::Function { .. } => top_funcs.push(*decl_id),
                _ => {
                    panic!("TODO, Un-handled Top-Level Declartion: {:?}", &decl.kind);
                }
            }
        }

        println!();
        println!("REPORT");
        println!("Top-level Structs");
        for decl_id in top_structs.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();
            if let CDeclKind::Struct { name, .. } = &decl.kind {
                println!("  {name:?}");
            } else {
                panic!("impossible");
            }
        }
        println!("Top-level Unions");
        for decl_id in top_unions.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();
            if let CDeclKind::Union { name, .. } = &decl.kind {
                println!("  {name:?}");
            } else {
                panic!("impossible");
            }
        }
        println!("Top-level TypeDefs");
        for decl_id in top_typedefs.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();
            if let CDeclKind::Typedef { name, .. } = &decl.kind {
                println!("  {name}");
            } else {
                panic!("impossible");
            }
        }
        println!("Top-level Variables");
        for decl_id in top_vars.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();
            if let CDeclKind::Variable { ident, .. } = &decl.kind {
                println!("  {ident}");
            } else {
                panic!("impossible");
            }
        }
        println!("Top-level Functions");
        for decl_id in top_funcs.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();
            if let CDeclKind::Function {
                name, is_global, ..
            } = &decl.kind
            {
                println!("  {name} {is_global}");
            } else {
                panic!("impossible");
            }
        }
    }
}
