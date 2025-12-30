use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    fmt::Display,
    fs::File,
    path::{Path, PathBuf},
    process::Command,
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
    let v: Vec<CompileCmd> = serde_json::from_reader(std::io::BufReader::new(
        File::open(base.join("compile_commands.json")).unwrap(),
    ))
    .unwrap();
    v.iter()
        .map(|cc| {
            ConversionContext::new(
                &c2rust_ast_exporter::get_untyped_ast(
                    &cc.file,
                    base,
                    &[],
                    false,
                )
                .unwrap(),
            )
            .typed_context
        })
        .collect()
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
            _ast: populate_from(cc_dir.path()),
        })
    }

    pub fn tree_crawl(&self) {
        tree_crawl::convert_root(&self._ast[0]);
    }
}

mod tree_crawl {
    use c2rust_transpile::c_ast::*;
    use std::collections::{HashSet, HashMap};
    use crate::hc_ast;
    use std::cell::{RefCell};

    // Hold the context data structures
    // that need to be passed everywhere during this
    // conversion pass
    struct CrawlPass<'a> {
        pub ctxt: &'a TypedAstContext,
        pub cunit: RefCell<hc_ast::CCompUnit>,
        // since we're translating cyclic data structures using IDs, we
        // need to remap the ids we're choosing to keep around
        pub f_remap: RefCell<HashMap<CDeclId, hc_ast::CFuncId>>,
        pub t_remap: RefCell<HashMap<CDeclId, hc_ast::CTypedefId>>,
        pub s_remap: RefCell<HashMap<CDeclId, hc_ast::CStructId>>,
        pub u_remap: RefCell<HashMap<CDeclId, hc_ast::CUnionId>>,
        pub e_remap: RefCell<HashMap<CDeclId, hc_ast::CEnumId>>,
        pub gv_remap: RefCell<HashMap<CDeclId, hc_ast::CGVarId>>,
    }

    pub fn convert_root(ctxt: &TypedAstContext) -> hc_ast::CCompUnit {
        let pass = CrawlPass{
            ctxt: ctxt,
            cunit: RefCell::new(hc_ast::CCompUnit::new()),
            f_remap: RefCell::new(HashMap::new()),
            t_remap: RefCell::new(HashMap::new()),
            s_remap: RefCell::new(HashMap::new()),
            u_remap: RefCell::new(HashMap::new()),
            e_remap: RefCell::new(HashMap::new()),
            gv_remap: RefCell::new(HashMap::new()),
        };
        convert_top_levels(&pass);
        pass.cunit.into_inner()
    }

    fn ok_to_discard_non_canonical_top_levels(ctxt: &TypedAstContext) {
        // Some extra code to make sure that we're not losing anything
        // by discarding non-canonical declarations
        let mut top_canons = HashSet::new();
        let mut non_canon_to_check = Vec::new();

        for decl_id in ctxt.c_decls_top.iter() {
            let decl = ctxt.get_decl(decl_id).unwrap();

            if let CDeclKind::NonCanonicalDecl { canonical_decl } = &decl.kind
            {
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

    fn convert_top_levels(pass: &CrawlPass) {
        // up front sanity checks on the C2Rust C AST
        ok_to_discard_non_canonical_top_levels(pass.ctxt);

        // HYPOTHESIS: implicit typedefs are always the same
        //             across all files; they simply reflect
        //             built-in compiler features
        //let mut implicit_typedefs: Vec<CDeclId> = Vec::new();

        // Q: Do we need to keep around the MacroObject or MacroFunction
        //      top-level declarations?  I think we'll need a more
        //      advanced macro-processing approach eventually anyway
        let mut top_macros: Vec<CDeclId> = Vec::new();

        // We make a first pass to reserve ids and establish
        // mappings for potentially recursive references to certain
        // kinds of top-level objects.
        for decl_id in pass.ctxt.c_decls_top.iter() {
            match &pass.ctxt[*decl_id].kind {
                CDeclKind::Function { .. } => {
                    let id = pass.cunit.borrow_mut().reserve_func();
                    pass.f_remap.borrow_mut().insert(*decl_id, id);
                }
                // skip implicit typedefs...
                //CDeclKind::Typedef { is_implicit: true, .. } => {},
                CDeclKind::Typedef { .. } => {
                    let id = pass.cunit.borrow_mut().reserve_typedef();
                    pass.t_remap.borrow_mut().insert(*decl_id, id);
                }
                CDeclKind::Struct { .. } => {
                    let id = pass.cunit.borrow_mut().reserve_struct();
                    pass.s_remap.borrow_mut().insert(*decl_id, id);
                }
                CDeclKind::Union { .. } => {
                    let id = pass.cunit.borrow_mut().reserve_union();
                    pass.u_remap.borrow_mut().insert(*decl_id, id);
                }
                CDeclKind::Enum { .. } => {
                    let id = pass.cunit.borrow_mut().reserve_enum();
                    pass.e_remap.borrow_mut().insert(*decl_id, id);
                }
                CDeclKind::Variable { .. } => {
                    let id = pass.cunit.borrow_mut().reserve_global_var();
                    pass.gv_remap.borrow_mut().insert(*decl_id, id);
                }
                _ => {}, // ignore most cases
            }
        }

        // iterate over the top-level declarations
        for decl_id in pass.ctxt.c_decls_top.iter() {
            let decl = &pass.ctxt[*decl_id];

            match &decl.kind {
                // top-levels that we'll cache just in case, but
                // which probably don't need to be used???
                CDeclKind::MacroObject { .. } => top_macros.push(*decl_id),
                CDeclKind::MacroFunction { .. } => top_macros.push(*decl_id),
                // our earlier check has certified that it's ok to
                // ignore the non-canonical declarations without losing
                // track of any top-level declarations in the process
                CDeclKind::NonCanonicalDecl { .. } => {}

                /*
                 *  Functions (populate body etc.)
                 */
                CDeclKind::Function {
                    is_inline_externally_visible: true, name, ..
                } => {
                    panic!("Found a function that uses the \
                            is_inline_externally_visible field: {name}");
                },
                CDeclKind::Function {
                    is_global, is_inline, is_implicit, is_extern,
                    is_inline_externally_visible: false,
                    typ, name, parameters, body: None, attrs: _,
                } => {
                    assert!(*is_global,
                        "Found a non-global function declaration: {name}");

                    let typ = convert_func_type(pass, *typ);
                    let params = parameters.iter()
                        .map(|p| convert_param(pass, *p))
                        .collect();

                    let id = pass.f_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_func(id,
                        hc_ast::CFuncDef{
                            name: name.clone(),
                            is_global: *is_global,
                            typ: typ,
                            params: params,
                            body: None,
                            annotations: hc_ast::CFuncAnnotations::new(
                                *is_extern, *is_inline, *is_implicit,
                            ),
                        });
                },
                // Process Function Definitions
                CDeclKind::Function {
                    is_global, is_inline, is_implicit, is_extern,
                    is_inline_externally_visible: false,
                    typ, name, parameters, body: Some(body_id), attrs: _,
                } => {
                    assert!(!*is_implicit,
                            "Found implicit function definition with \
                             a body: {name}");
                    assert!(*is_global || !*is_extern,
                        "Found a function that is marked as extern \
                         but is somehow not global: {name}");

                    let body = convert_stmt(pass, *body_id);
                    //let body = Box::new(hc_ast::CStmt::Noop);
                    let typ = convert_func_type(pass, *typ);
                    let params = parameters.iter()
                        .map(|p| convert_param(pass, *p))
                        .collect();

                    let id = pass.f_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_func(id,
                        hc_ast::CFuncDef{
                            name: name.clone(),
                            is_global: *is_global,
                            typ: typ,
                            params: params,
                            body: Some(body),
                            annotations: hc_ast::CFuncAnnotations::new(
                                *is_extern, *is_inline, *is_implicit,
                            ),
                        });
                },

                /*
                 *  Typedefs
                 */
                CDeclKind::Typedef {
                    name, typ: qtyp, is_implicit
                } => {
                    let id = pass.t_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_typedef(id,
                        hc_ast::CTypedef{
                            name: name.clone(),
                            typ: convert_qual_type(pass, *qtyp),
                            is_implicit: *is_implicit,
                        });
                },

                /*
                 *  Structs (populate fields)
                 */
                CDeclKind::Struct {
                    name: Some(name), fields:None,
                    is_packed: false,
                    manual_alignment: None, max_field_alignment: None,
                    platform_byte_size: 0, platform_alignment: 0,
                } => {
                    let id = pass.s_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_struct(id,
                        hc_ast::CStructDef::Decl( name.clone() ));
                }
                CDeclKind::Struct { fields: None, .. } => {
                    panic!("Pure declarations of top-level structures \
                        should have names, not be packed, and should \
                        have no alignment or size data; found a violation")
                }
                CDeclKind::Struct { fields: Some(_), .. } => {
                    let cstruct = convert_struct(pass, *decl_id);
                    let id = pass.s_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_struct(id,
                        hc_ast::CStructDef::Defn(cstruct));
                }

                /*
                 *  Unions (populate fields)
                 */
                CDeclKind::Union {
                    name: Some(name), fields:None,
                    is_packed: false,
                } => {
                    let id = pass.u_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_union(id,
                        hc_ast::CUnionDef::Decl( name.clone() ));
                }
                CDeclKind::Union { fields: None, .. } => {
                    panic!("Pure declarations of top-level unions \
                        should have names and not be packed")
                }
                CDeclKind::Union { fields:Some(_), .. } => {
                    let cunion = convert_union(pass, *decl_id);
                    let id = pass.u_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_union(id,
                        hc_ast::CUnionDef::Defn(cunion));
                }

                /*
                 *  Enums (populate fields)
                 */
                CDeclKind::Enum { .. } => {
                    let cenum = convert_enum(pass, *decl_id);
                    let id = pass.e_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_enum(id, cenum);
                }

                /*
                 *  Global Variables
                 */
                CDeclKind::Variable {
                    has_static_duration: true, has_thread_duration: false,
                    is_externally_visible: true, is_defn: false,
                    ident, initializer: None, typ, attrs: _,
                } => {
                    let id = pass.gv_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_global_var(id,
                        hc_ast::CGlobalVar::Decl{
                            name: ident.clone(),
                            typ: convert_qual_type(pass, *typ),
                        });
                }
                CDeclKind::Variable {
                    has_static_duration: true, has_thread_duration: false,
                    is_externally_visible, is_defn: true,
                    ident, initializer, typ, attrs: _,
                } => {
                    // this was not true
                    //if *is_externally_visible {
                    //    assert!(initializer.is_some());
                    //}
                    let init = match initializer {
                        Some(eid) => Some(convert_expr(pass, *eid)),
                        None => None,
                    };
                    let id = pass.gv_remap.borrow()[decl_id];
                    pass.cunit.borrow_mut().add_global_var(id,
                        hc_ast::CGlobalVar::Defn{
                            name: ident.clone(),
                            typ: convert_qual_type(pass, *typ),
                            init: init,
                            is_extern: *is_externally_visible,
                        });
                }
                CDeclKind::Variable { .. } => {
                    panic!("detected unexpected global variable");
                }

                /*
                 *  Errors
                 */
                _ => {
                    panic!(
                        "TODO, Unhandled Top-Level Declartion: {:?}",
                        &decl.kind
                    );
                }
            }
        }

        // Post-Hoc validation of the constructed AST
        pass.cunit.borrow().validate();
        //pass.cunit.borrow().print_report();
    }

    fn convert_param(
        pass: &CrawlPass, id: CParamId,
    ) -> hc_ast::CParam {
        let decl = &pass.ctxt[id];
        let CDeclKind::Variable {
            has_static_duration: false, has_thread_duration: false,
            is_externally_visible: false, is_defn: true,
            ident, initializer: None, typ, attrs: _,
        } = &decl.kind else {
            panic!("expected Variable CDecl with certain properties");
        };

        hc_ast::CParam{
            name: ident.clone(),
            typ: convert_qual_type(pass, *typ),
        }
    }

    fn convert_struct(
        pass: &CrawlPass, id: CDeclId,
    ) -> hc_ast::CStruct {
        let decl = &pass.ctxt[id];
        let CDeclKind::Struct {
            name, fields: Some(flds),
            is_packed, manual_alignment, max_field_alignment,
            platform_byte_size, platform_alignment,
        } = &decl.kind else { panic!("expected Struct CDecl"); };

        let flds = flds.iter()
                        .map(|fid| convert_field(pass, *fid))
                        .collect();
        hc_ast::CStruct{
            name: name.clone(),
            fields: flds,
            align: hc_ast::CStructAlignment{
                is_packed: *is_packed,
                manual_alignment: *manual_alignment,
                max_field_alignment: *max_field_alignment,
                platform_byte_size: *platform_byte_size,
                platform_alignment: *platform_alignment,
            }
        }
    }

    fn convert_union(
        pass: &CrawlPass, id: CDeclId,
    ) -> hc_ast::CUnion {
        let decl = &pass.ctxt[id];
        let CDeclKind::Union {
            name, fields:Some(flds), is_packed,
        } = &decl.kind else { panic!("expected Union CDecl"); };

        let flds = flds.iter()
                        .map(|fid| convert_field(pass, *fid))
                        .collect();
        hc_ast::CUnion{
            name: name.clone(),
            fields: flds,
            is_packed: *is_packed,
        }
    }


    fn convert_enum(
        pass: &CrawlPass, id: CDeclId,
    ) -> hc_ast::CEnumDef {
        let decl = &pass.ctxt[id];
        let CDeclKind::Enum {
            name, variants, integral_type: _int_type,
        } = &decl.kind else { panic!("expected Enum CDecl"); };

        let consts = variants.iter()
            .map(|ecid| convert_enum_const(pass, *ecid))
            .collect();
        hc_ast::CEnumDef{
            name: name.clone(),
            consts: consts,
        }
    }

    fn convert_field(
        pass: &CrawlPass, fid: CFieldId
    ) -> hc_ast::CField {
        let CDeclKind::Field{
            name, typ, ..
        } = &pass.ctxt[fid].kind else {
            panic!("expected a field");
        };
        let typ = convert_qual_type(pass, *typ);
        hc_ast::CField{ name: name.clone(), typ: typ }
    }

    fn convert_enum_const(
        pass: &CrawlPass, ecid: CEnumConstantId,
    ) -> hc_ast::CEnumConst {
        let CDeclKind::EnumConstant{
            name, value
        } = &pass.ctxt[ecid].kind else {
            panic!("expected an EnumConstant");
        };
        let val = match value {
            ConstIntExpr::U(v) => hc_ast::IntLiteralValue::ULit(*v),
            ConstIntExpr::I(v) => hc_ast::IntLiteralValue::ILit(*v),
        };
        hc_ast::CEnumConst{name: name.clone(), value: val }
    }

    fn convert_stmt(
        pass: &CrawlPass, stmt_id: CStmtId,
    ) -> Box<hc_ast::CStmt> {
        let stmt = &pass.ctxt[stmt_id];
        match &stmt.kind {
            CStmtKind::Compound(stmts) => {
                Box::new(hc_ast::CStmt::Block(stmts.iter()
                    .map(|s| convert_stmt(pass, *s)).collect()))
            }
            CStmtKind::Empty => { Box::new(hc_ast::CStmt::Noop) }
            CStmtKind::If{ scrutinee, true_variant, false_variant } => {
                let cond = convert_expr(pass, *scrutinee);
                let tcase = convert_stmt(pass, *true_variant);
                let fcase = false_variant.map(|fv| convert_stmt(pass, fv));
                Box::new(hc_ast::CStmt::If(cond, tcase, fcase))
            }
            CStmtKind::While{ condition, body } => {
                let cond = convert_expr(pass, *condition);
                let body = convert_stmt(pass, *body);
                Box::new(hc_ast::CStmt::While(cond, body))
            }
            CStmtKind::DoWhile{ body, condition } => {
                let body = convert_stmt(pass, *body);
                let cond = convert_expr(pass, *condition);
                Box::new(hc_ast::CStmt::DoWhile(body, cond))
            }
            CStmtKind::ForLoop{ init, condition, increment, body } => {
                let init = init.map(|i| convert_stmt(pass, i));
                let cond = condition.map(|c| convert_expr(pass, c));
                let inc = increment.map(|i| convert_expr(pass, i));
                let body = convert_stmt(pass, *body);
                Box::new(hc_ast::CStmt::ForLoop(init, cond, inc, body))
            }
            CStmtKind::Break => {
                Box::new(hc_ast::CStmt::Break)
            },
            CStmtKind::Continue => {
                Box::new(hc_ast::CStmt::Continue)
            },
            CStmtKind::Expr(expr) => {
                Box::new(hc_ast::CStmt::Expr(convert_expr(pass, *expr)))
            },
            CStmtKind::Return(expr) => {
                let expr = expr.map(|e| convert_expr(pass, e));
                Box::new(hc_ast::CStmt::Return(expr))
            },
            CStmtKind::Decls(_) => {
                Box::new(hc_ast::CStmt::Noop) // TODO: REPLACE
            }
            CStmtKind::Goto(_) => {
                Box::new(hc_ast::CStmt::Noop) // TODO: REPLACE
            }
            CStmtKind::Label(_) => {
                Box::new(hc_ast::CStmt::Noop) // TODO: REPLACE
            }
            CStmtKind::Switch{ .. } => {
                Box::new(hc_ast::CStmt::Noop) // TODO: REPLACE
            }
            _ => {
                panic!("TODO, Unhandled Stmt: {:?}", stmt.kind);
            }
        }
    }

    fn convert_expr(
        pass: &CrawlPass, expr_id: CExprId,
    ) -> Box<hc_ast::CExpr> {
        let expr = &pass.ctxt[expr_id];
        match &expr.kind {
            CExprKind::Literal(qt, lit) => {
                let lit = convert_literal(lit);
                let qtype = convert_qual_type(pass, *qt);
                Box::new(hc_ast::CExpr::Literal(qtype, lit))
            }
            CExprKind::Unary(qt, op, arg, lr) => {
                let op = match op {
                    UnOp::AddressOf => hc_ast::UnaryOp::AddressOf,
                    UnOp::Deref => hc_ast::UnaryOp::Deref,
                    UnOp::Plus => hc_ast::UnaryOp::Plus,
                    UnOp::PostIncrement => hc_ast::UnaryOp::PostInc,
                    UnOp::PreIncrement => hc_ast::UnaryOp::PreInc,
                    UnOp::Negate => hc_ast::UnaryOp::Minus,
                    UnOp::PostDecrement => hc_ast::UnaryOp::PostDec,
                    UnOp::PreDecrement => hc_ast::UnaryOp::PreDec,
                    UnOp::Complement => hc_ast::UnaryOp::BitNot,
                    UnOp::Not => hc_ast::UnaryOp::Not,
                    UnOp::Real => hc_ast::UnaryOp::Real,
                    UnOp::Imag => hc_ast::UnaryOp::Imag,
                    UnOp::Extension => hc_ast::UnaryOp::Extension,
                    UnOp::Coawait => { panic!("coawait is C++ only"); }
                };
                let qtype = convert_qual_type(pass, *qt);
                let arg = convert_expr(pass, *arg);
                Box::new(hc_ast::CExpr::Unary(qtype, op, arg, to_lval(lr)))
            }
            CExprKind::Binary(qt, op, lhs, rhs, ltyp, rtyp) => {
                let op = match op {
                    BinOp::Multiply => hc_ast::BinOp::Mult,
                    BinOp::Divide => hc_ast::BinOp::Div,
                    BinOp::Modulus => hc_ast::BinOp::Mod,
                    BinOp::Add => hc_ast::BinOp::Add,
                    BinOp::Subtract => hc_ast::BinOp::Sub,
                    BinOp::ShiftLeft => hc_ast::BinOp::ShiftL,
                    BinOp::ShiftRight => hc_ast::BinOp::ShiftR,
                    BinOp::Less => hc_ast::BinOp::Lt,
                    BinOp::Greater => hc_ast::BinOp::Gt,
                    BinOp::LessEqual => hc_ast::BinOp::Le,
                    BinOp::GreaterEqual => hc_ast::BinOp::Ge,
                    BinOp::EqualEqual => hc_ast::BinOp::Eq,
                    BinOp::NotEqual => hc_ast::BinOp::Neq,
                    BinOp::BitAnd => hc_ast::BinOp::BitAnd,
                    BinOp::BitXor => hc_ast::BinOp::BitXor,
                    BinOp::BitOr => hc_ast::BinOp::BitOr,
                    BinOp::And => hc_ast::BinOp::And,
                    BinOp::Or => hc_ast::BinOp::Or,
                    BinOp::AssignAdd => hc_ast::BinOp::AssignAdd,
                    BinOp::AssignSubtract => hc_ast::BinOp::AssignSub,
                    BinOp::AssignMultiply => hc_ast::BinOp::AssignMult,
                    BinOp::AssignDivide => hc_ast::BinOp::AssignDiv,
                    BinOp::AssignModulus => hc_ast::BinOp::AssignMod,
                    BinOp::AssignBitXor => hc_ast::BinOp::AssignBitXor,
                    BinOp::AssignShiftLeft => hc_ast::BinOp::AssignShiftL,
                    BinOp::AssignShiftRight => hc_ast::BinOp::AssignShiftR,
                    BinOp::AssignBitOr => hc_ast::BinOp::AssignBitOr,
                    BinOp::AssignBitAnd => hc_ast::BinOp::AssignBitAnd,
                    BinOp::Assign => hc_ast::BinOp::Assign,
                    BinOp::Comma => hc_ast::BinOp::Comma,
                };
                let qtype = convert_qual_type(pass, *qt);
                let lhs = convert_expr(pass, *lhs);
                let rhs = convert_expr(pass, *rhs);
                let ltyp = ltyp.map(|lt| convert_qual_type(pass, lt));
                let rtyp = rtyp.map(|rt| convert_qual_type(pass, rt));
                match ltyp {
                    Some(lt) => assert!(lt == *lhs.ret_type())
                };
                //assert!(rtyp == rhs.ret_type());
                // TODO: CHECK IF QUAL TYPES ARE JUST LEFT AND RIGHT
                // ARGUMENT QUAL TYPES, and if so CAN WE DROP?
                Box::new(hc_ast::CExpr::Binary(
                    qtype, op, lhs, rhs, ltyp, rtyp))
            }
            CExprKind::UnaryType(qt, op, earg, argtyp) => {
                let qtype = convert_qual_type(pass, *qt);
                let earg = earg.map(|e| convert_expr(pass, e));
                let argtyp = convert_qual_type(pass, *argtyp);
                let op = match op {
                    UnTypeOp::SizeOf => hc_ast::UOfTypeOp::SizeOf,
                    UnTypeOp::AlignOf => hc_ast::UOfTypeOp::AlignOf,
                    UnTypeOp::PreferredAlignOf => panic!("Found preferred alignof"),
                };
                Box::new(hc_ast::CExpr::UnaryOfType(qtype, op, earg, argtyp))
            }
            CExprKind::ImplicitCast(qt, arg, _cast_kind, _field, lr) => {
                let qtype = convert_qual_type(pass, *qt);
                let arg = convert_expr(pass, *arg);
                Box::new(hc_ast::CExpr::ImplicitCast(qtype, arg, to_lval(lr)))
            }
            CExprKind::ExplicitCast(qt, arg, _cast_kind, _field, lr) => {
                let qtype = convert_qual_type(pass, *qt);
                let arg = convert_expr(pass, *arg);
                Box::new(hc_ast::CExpr::ExplicitCast(qtype, arg, to_lval(lr)))
            }

            // TODO: ConstantExpr

            // leaf nodes that are names
            CExprKind::DeclRef(qt, decl_id, lr) => {
                let qtype = convert_qual_type(pass, *qt);
                let decl = &pass.ctxt[*decl_id];
                match &decl.kind {
                    CDeclKind::Variable { ident, attrs: _, .. } => {
                        if let Some(gvid) =
                            pass.gv_remap.borrow().get(decl_id)
                        {
                            Box::new(hc_ast::CExpr::GlobalVar(
                                qtype, *gvid, ident.clone(), to_lval(lr)))
                        } else {
                            Box::new(hc_ast::CExpr::LocalVar(
                                qtype, ident.clone(), to_lval(lr)))
                        }
                    }
                    CDeclKind::Function { name, attrs: _, .. } => {
                        let func_id = *pass.f_remap.borrow().get(decl_id)
                        .expect(
                            &format!("could not find func id: {decl_id:?}")
                        );
                        Box::new(hc_ast::CExpr::FuncName(
                            qtype, func_id, name.clone(), to_lval(lr)))
                    }
                    CDeclKind::EnumConstant { .. } => {
                        let hc_ast::CEnumConst{name, value} =
                            convert_enum_const(pass, *decl_id);
                        Box::new(hc_ast::CExpr::EnumConst(
                            qtype, name, value, to_lval(lr)))
                    }
                    _ => { panic!("unhandled DeclRef() case: {decl:?}"); }
                }
            }

            CExprKind::Call(qt, func, args) => {
                let qtype = convert_qual_type(pass, *qt);
                let func = convert_expr(pass, *func);
                let args = args.iter()
                    .map(|a| convert_expr(pass, *a)).collect();
                Box::new(hc_ast::CExpr::Call(qtype, func, args))
            }
            CExprKind::Member(qt, baseid, fid, kind, lr) => {
                let qtype = convert_qual_type(pass, *qt);
                let base = convert_expr(pass, *baseid);
                assert!(matches!(pass.ctxt[*fid].kind, CDeclKind::Field{..}),
                    "expected Member() to refer to a Field declaration");
                // extract the type and struct decl from the base expression
                let tid = &pass.ctxt[*baseid].kind
                    .get_qual_type().unwrap().ctype;

                // get the field sequence id and whether this was a
                // struct or union field access
                let (f_pos, obj_typ) = find_field(pass, tid, fid, kind);
                match (kind, obj_typ) {
                    (MemberKind::Arrow, FieldIn::Struct) => {
                        Box::new(hc_ast::CExpr::SFieldArrow(
                            qtype, base, f_pos, to_lval(lr)))
                    },
                    (MemberKind::Arrow, FieldIn::Union) => {
                        Box::new(hc_ast::CExpr::UFieldArrow(
                            qtype, base, f_pos, to_lval(lr)))
                    },
                    (MemberKind::Dot, FieldIn::Struct) => {
                        Box::new(hc_ast::CExpr::SFieldDot(
                            qtype, base, f_pos, to_lval(lr)))
                    },
                    (MemberKind::Dot, FieldIn::Union) => {
                        Box::new(hc_ast::CExpr::UFieldDot(
                            qtype, base, f_pos, to_lval(lr)))
                    },
                }
            }
            CExprKind::ArraySubscript(qt, base, idx, lr) => {
                let qtype = convert_qual_type(pass, *qt);
                let base = convert_expr(pass, *base);
                let idx = convert_expr(pass, *idx);
                Box::new(hc_ast::CExpr::ArrayIndex(
                    qtype, base, idx, to_lval(lr)))
            }
            CExprKind::Conditional(qt, cond, tcase, fcase) => {
                let qtype = convert_qual_type(pass, *qt);
                let cond = convert_expr(pass, *cond);
                let tcase = convert_expr(pass, *tcase);
                let fcase = convert_expr(pass, *fcase);
                Box::new(hc_ast::CExpr::Ternary(qtype, cond, tcase, fcase))
            }
            CExprKind::BinaryConditional(..) => {
                unimplemented!("GNU Binary Conditional Operator");
            }

            CExprKind::InitList(..) => { convert_init_list(pass, expr) }
            CExprKind::DesignatedInitExpr(..) => {
                panic!("DesignatedInitExpr() should only occur right inside
                        an InitList() node");
            }

            _ => {
                panic!("TODO, Unhandled Expr: {:?}", expr.kind);
            }
        }
    }

    use c2rust_ast_exporter::clang_ast::LRValue;
    fn to_lval(lr: &LRValue) -> hc_ast::LVal {
        match lr { LRValue::LValue => true, LRValue::RValue => false }
    }


    enum FieldIn { Struct, Union }
    fn find_field(
        pass: &CrawlPass, tid: &CTypeId, fid: &CDeclId, kind: &MemberKind
    ) -> (u32, FieldIn) {
        // get the underlying struct or union type id
        let tid = match kind {
            MemberKind::Arrow => {
                let CTypeKind::Pointer(qt)
                    = &pass.ctxt.resolve_type(*tid).kind
                    else { panic!("expected type to be a pointer") };
                &qt.ctype
            },
            MemberKind::Dot => tid,
        };
        // get the fields and record whether this was a struct or union
        let tkind = &pass.ctxt.resolve_type(*tid).kind;
        let (flds, is_in) = match tkind {
            CTypeKind::Struct(sid) => {
                let CDeclKind::Struct{fields: Some(flds), ..}
                            = &pass.ctxt[*sid].kind
                else { panic!("expected a Struct() type definition") };
                (flds, FieldIn::Struct)
            },
            CTypeKind::Union(uid) => {
                let CDeclKind::Union{fields: Some(flds), ..}
                            = &pass.ctxt[*uid].kind
                else { panic!("expected a Union() type definition") };
                (flds, FieldIn::Union)
            },
            _ => { panic!("expected type to be a struct or union {:?}",
                          tkind); }
        };

        // finally, find which field is being accessed
        let Some(f_pos) = flds.iter().position(|f| f == fid)
            else { panic!("expected to find field") };

        ( f_pos.try_into().unwrap(), is_in )
    }


    enum TempD { I(u64), SF(u32), UF(u32) }
    fn convert_init_entry(
        pass: &CrawlPass, mut tid: CTypeId, expr_id: CExprId,
    ) -> Box<hc_ast::InitListEntry> {
        let expr = &pass.ctxt[expr_id];
        if let CExprKind::DesignatedInitExpr(_qt, ds, init_e) = &expr.kind {
            // NOTE: is the lack of carrying over the qtype a sign of
            //       a mistake in our IR design?  (got interrupted)
            //let qtype = convert_qual_type(pass, *qt);
            let init_e = convert_expr(pass, *init_e);
            let mut entry = Box::new(hc_ast::InitListEntry::Val(init_e));

            let mut tmp_ds = Vec::new();
            // process the stack of initialization designators so that we can
            // keep track of the type being accessed by each designator
            // This allows us to convert `CFieldId`s into u32 indices
            // (`f_pos`) identifying the field by occurrence in the struct
            for d in ds { match d {
                Designator::Index(idx) => {
                    let CTypeKind::ConstantArray(a_tid, _size)
                        = &pass.ctxt.resolve_type(tid).kind
                    else { panic!("expected ConstantArray() type") };
                    tmp_ds.push(TempD::I(*idx));
                    tid = *a_tid;
                }
                Designator::Field(fid) => {
                    let CDeclKind::Field{ typ, .. } = &pass.ctxt[*fid].kind
                    else { panic!("expected field declaration") };
                    let (f_pos, obj_typ) =
                        find_field(pass, &tid, fid, &MemberKind::Dot);
                    match obj_typ {
                        FieldIn::Struct => { tmp_ds.push(TempD::SF(f_pos)); }
                        FieldIn::Union => { tmp_ds.push(TempD::UF(f_pos)); }
                    }
                    tid = typ.ctype;
                }
                Designator::Range(_,_) => {
                    unimplemented!("GNU C designated range initializers");
                }
            } }

            use hc_ast::InitListEntry::*;
            for d in tmp_ds.into_iter().rev() { match d {
                TempD::I(idx) => {
                    entry = Box::new(IndexDesignation(idx, entry));
                }
                TempD::SF(fpos) => {
                    entry = Box::new(StructDesignation(fpos, entry));
                }
                TempD::UF(fpos) => {
                    entry = Box::new(UnionDesignation(fpos, entry));
                }
            } }

            return entry;
        } else {
            let e = convert_expr(pass, expr_id);
            return Box::new(hc_ast::InitListEntry::Val(e));
        }
    }

    fn convert_init_list(
        pass: &CrawlPass, initlist: &CExpr,
    ) -> Box<hc_ast::CExpr> {
        // NOTE: the version of c2rust's parser that we are using does
        //       produce `Some(_syntax_form)` but when it does produce
        //       such data, it is invalid (i.e. contains dangling references)
        //       Therefore we discard that data
        let CExprKind::InitList(qt, entries, None, _sform) = &initlist.kind
        else {
            unimplemented!("InitList support for
                            'union field' (Option<CFieldId>): {:?}",
                            initlist.kind);
        };
        let base_tid = &qt.ctype;
        let qtype = convert_qual_type(pass, *qt);
        let entries = entries.iter()
            .map(|e| convert_init_entry(pass, *base_tid, *e)).collect();
        Box::new(hc_ast::CExpr::InitList(qtype, entries))
    }

    fn convert_literal(lit: &CLiteral) -> hc_ast::CLiteral {
        match lit {
            CLiteral::Integer(v,base) => {
                let base = match base {
                    IntBase::Dec => hc_ast::CIntBase::Dec,
                    IntBase::Hex => hc_ast::CIntBase::Hex,
                    IntBase::Oct => hc_ast::CIntBase::Oct,
                };
                hc_ast::CLiteral::I(*v,base)
            },
            CLiteral::Character(v) => hc_ast::CLiteral::C(*v),
            CLiteral::Floating(v, s) => hc_ast::CLiteral::F(*v, s.clone()),
            CLiteral::String(b, nbytes) =>
                hc_ast::CLiteral::S(b.clone(), *nbytes),
        }
    }

    fn convert_func_type(
        pass: &CrawlPass, ftid: CFuncTypeId,
    ) -> hc_ast::CFuncType {
        let ctyp = &pass.ctxt[ftid];
        let CTypeKind::Function (
            rtyp, ptyps, is_vararg, is_noreturn, has_proto
        ) = &ctyp.kind else { panic!("expected Function CType"); };

        let rtyp = convert_qual_type(pass, *rtyp);
        let ptyps = ptyps.iter()
            .map(|p| convert_qual_type(pass, *p))
            .collect();

        hc_ast::CFuncType{
            rtyp: rtyp,
            ptyps: ptyps,
            annotations: hc_ast::CFuncTypeAnnotations::new(
                *is_vararg, *is_noreturn, *has_proto
            ),
        }
    }

    fn convert_qual_type(
        pass: &CrawlPass, qtype: CQualTypeId,
    ) -> hc_ast::CQualType {
        hc_ast::CQualType {
            typ: convert_type(pass, qtype.ctype),
            quals: hc_ast::CTypeQualifiers::new(
                qtype.qualifiers.is_const,
                qtype.qualifiers.is_restrict,
                qtype.qualifiers.is_volatile),
        }
    }

    // helper function for convert_type
    fn prim(pt: hc_ast::CPrimType) -> Box<hc_ast::CType> {
        Box::new(hc_ast::CType::Prim(pt))
    }

    fn convert_type(
        pass: &CrawlPass, type_id: CTypeId,
    ) -> Box<hc_ast::CType> {
        let orig_typ = &pass.ctxt[type_id];
        match &orig_typ.kind {
            CTypeKind::Void => Box::new(hc_ast::CType::Void),

            CTypeKind::Bool => prim(hc_ast::CPrimType::Bool),
            CTypeKind::Char => prim(hc_ast::CPrimType::Char),

            CTypeKind::SChar => prim(hc_ast::CPrimType::SChar),
            CTypeKind::Short => prim(hc_ast::CPrimType::Short),
            CTypeKind::Int => prim(hc_ast::CPrimType::Int),
            CTypeKind::Long => prim(hc_ast::CPrimType::Long),
            CTypeKind::LongLong => prim(hc_ast::CPrimType::LongLong),

            CTypeKind::UChar => prim(hc_ast::CPrimType::UChar),
            CTypeKind::UShort => prim(hc_ast::CPrimType::UShort),
            CTypeKind::UInt => prim(hc_ast::CPrimType::UInt),
            CTypeKind::ULong => prim(hc_ast::CPrimType::ULong),
            CTypeKind::ULongLong => prim(hc_ast::CPrimType::ULongLong),

            CTypeKind::Int128 => prim(hc_ast::CPrimType::Int128),
            CTypeKind::UInt128 => prim(hc_ast::CPrimType::UInt128),

            CTypeKind::Float => prim(hc_ast::CPrimType::Float),
            CTypeKind::Double => prim(hc_ast::CPrimType::Double),
            CTypeKind::LongDouble => prim(hc_ast::CPrimType::LongDouble),
            CTypeKind::Half => prim(hc_ast::CPrimType::Half),
            CTypeKind::BFloat16 => prim(hc_ast::CPrimType::BFloat16),

            CTypeKind::Complex(tid) => {
                Box::new(hc_ast::CType::Complex(
                    convert_prim_type(pass, *tid)
                ))
            },

            // pass-through, erase parentheses for now
            CTypeKind::Paren(tid) => { convert_type(pass, *tid) },

            CTypeKind::ConstantArray(tid, size) => {
                let typ = convert_type(pass, *tid);
                Box::new(hc_ast::CType::ConstSizeArray(typ, *size))
            },
            CTypeKind::IncompleteArray(tid) => {
                let typ = convert_type(pass, *tid);
                Box::new(hc_ast::CType::NoSizeArray(typ))
            },
            CTypeKind::VariableArray(tid, sz_expr) => {
                let typ = convert_type(pass, *tid);
                let sz_expr = sz_expr.map(|e| convert_expr(pass, e));
                Box::new(hc_ast::CType::VarSizeArray(typ, sz_expr))
            },

            CTypeKind::Pointer(qtid) => {
                let qtyp = convert_qual_type(pass, *qtid);
                Box::new(hc_ast::CType::Ptr(qtyp))
            },

            CTypeKind::Elaborated(tid) => {
                // HYPOTHESIS:
                // should contain only Struct, Union, Enum, or Typedef
                let etyp = &pass.ctxt[*tid];
                match &etyp.kind {
                    CTypeKind::Struct(_) | CTypeKind::Union(_) |
                    CTypeKind::Enum(_) | CTypeKind::Typedef(_) => {
                        convert_type(pass, *tid)
                    },
                    /*
                    CTypeKind::Struct(sid) => {
                        let id = pass.s_remap.borrow().get(sid).expect(
                            &format!("could not find struct id: {etyp:?}")
                        ).clone();
                        Box::new(hc_ast::CType::StructDef(id))
                    },
                    CTypeKind::Union(uid) => {
                        return convert_type(pass, *tid);/*
                        println!("ELABORATED {:?} {:?} {:?}", tid, &uid, orig_typ.loc);
                        let id = pass.u_remap.borrow().get(uid).expect(
                            &format!("could not find union id: {etyp:?}")
                        ).clone();
                        Box::new(hc_ast::CType::UnionDef(id))*/
                    },
                    CTypeKind::Enum(eid) => {
                        let id = pass.e_remap.borrow().get(eid).expect(
                            &format!("could not find enum id: {etyp:?}")
                        ).clone();
                        Box::new(hc_ast::CType::EnumDef(id))
                    },
                    CTypeKind::Typedef(tdid) => {
                        let id = pass.t_remap.borrow().get(tdid).expect(
                            &format!("could not find typedef id: {etyp:?}")
                        ).clone();
                        Box::new(hc_ast::CType::Typedef(id))
                    },
                    */
                    _ => { panic!("Unexpected Elaborated() type, {etyp:?}"); }
                }
            },

            /* for the following, unlike the above elaborated
               type references, we expect that the definition should
               be inline, and thus NOT available at the top level
             */
            CTypeKind::Struct(id) => {
                match pass.s_remap.borrow().get(id) {
                    Some(sid) => { Box::new(hc_ast::CType::StructDef(*sid)) },
                    None => { Box::new(hc_ast::CType::Struct(
                                convert_struct(pass, *id))) },
                }
                /*
                if pass.s_remap.borrow().get(id).is_some() {
                    panic!("expected struct undefined at top level");
                }
                Box::new(hc_ast::CType::Struct(convert_struct(pass, *id)))*/
            },
            CTypeKind::Union(id) => {
                match pass.u_remap.borrow().get(id) {
                    Some(uid) => { Box::new(hc_ast::CType::UnionDef(*uid)) },
                    None => { Box::new(hc_ast::CType::Union(
                                convert_union(pass, *id))) },
                }/*
                        let id = pass.u_remap.borrow().get(uid).expect(
                            &format!("could not find union id: {etyp:?}")
                        ).clone();
                        Box::new(hc_ast::CType::UnionDef(id))

                if pass.u_remap.borrow().get(id).is_some() {
                    panic!("expected union undefined at top level");
                }
                Box::new(hc_ast::CType::Union(convert_union(pass, *id)))
                */
            },
            CTypeKind::Enum(id) => {
                match pass.e_remap.borrow().get(id) {
                    Some(eid) => { Box::new(hc_ast::CType::EnumDef(*eid)) },
                    None => { Box::new(hc_ast::CType::Enum(
                                convert_enum(pass, *id))) },
                }
                /*
                if pass.e_remap.borrow().get(id).is_some() {
                    panic!("expected enum undefined at top level");
                }
                Box::new(hc_ast::CType::Enum(convert_enum(pass, *id))) */
            },
            // unlike the other 3 cases, typedefs may only occur as
            // references to top-level definitions
            CTypeKind::Typedef(id) => {
                let tdid = pass.t_remap.borrow().get(id).expect(
                    &format!("could not find typedef id: {id:?}")
                ).clone();
                Box::new(hc_ast::CType::Typedef(tdid))
            },

            CTypeKind::Function(..) => {
                Box::new(hc_ast::CType::Func(
                    convert_func_type(pass, type_id)))
            },
            CTypeKind::BuiltinFn => Box::new(hc_ast::CType::BuiltinFn),

            CTypeKind::Decayed(tid) => {
                Box::new(hc_ast::CType::Decayed(convert_type(pass, *tid)))
            },

            CTypeKind::Vector(_qtid, _size) => {
                Box::new(hc_ast::CType::Unimplemented(String::from("Vector")))
            },
            CTypeKind::UnhandledSveType => {
                Box::new(hc_ast::CType::Unimplemented(String::from("Sve")))
            },
            CTypeKind::Reference(_) => {
                panic!("Did not expect Reference Types. \
                        I thought those were C++ only.")
            },
            _ => {
                panic!("TODO, Unhandled Type: {:?}", orig_typ.kind);
            },
        }
    }
    fn convert_prim_type(
        pass: &CrawlPass, type_id: CTypeId,
    ) -> hc_ast::CPrimType {
        let typbox = convert_type(pass, type_id);
        let hc_ast::CType::Prim(pt) = *typbox else {
            panic!("expected a primitive type");
        };
        pt
    }
}



