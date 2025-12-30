/*
 *  This file contains suport functions for
 *  HARVEST-IR's version of C ASTs.
 *
 *  Original Intentions when creating this file:
 *  - The support/helper functions for the IR became much larger than
 *    the data structure definitions, so I moved it into a separate file.
 *
 *
 */

use crate::hc_ast::*;
use std::ops::Index;

/*
 *
 *  Support for flattened recursion on Structs
 *
 */

impl Index<CFuncId> for CCompUnit {
  type Output = CFuncDef;

  fn index(&self, index: CFuncId) -> &CFuncDef {
    self.funcs.get(&index).expect(
      &format!("could not find function id: {index:?}")
    )
  }
}

impl Index<CTypedefId> for CCompUnit {
  type Output = CTypedef;

  fn index(&self, index: CTypedefId) -> &CTypedef {
    self.typedefs.get(&index).expect(
      &format!("could not find typedef id: {index:?}")
    )
  }
}

impl Index<CStructId> for CCompUnit {
  type Output = CStructDef;

  fn index(&self, index: CStructId) -> &CStructDef {
    self.structs.get(&index).expect(
      &format!("could not find struct id: {index:?}")
    )
  }
}

impl Index<CUnionId> for CCompUnit {
  type Output = CUnionDef;

  fn index(&self, index: CUnionId) -> &CUnionDef {
    self.unions.get(&index).expect(
      &format!("could not find union id: {index:?}")
    )
  }
}

impl Index<CEnumId> for CCompUnit {
  type Output = CEnumDef;

  fn index(&self, index: CEnumId) -> &CEnumDef {
    self.enums.get(&index).expect(
      &format!("could not find enum id: {index:?}")
    )
  }
}

impl Index<CGVarId> for CCompUnit {
  type Output = CGlobalVar;

  fn index(&self, index: CGVarId) -> &CGlobalVar {
    self.global_vars.get(&index).expect(
      &format!("could not find global variable id: {index:?}")
    )
  }
}

impl CCompUnit {
  pub fn reserve_func(&mut self) -> CFuncId {
    let id = CFuncId(self.func_id_count);
    self.func_id_count += 1;
    id
  }
  pub fn add_func(&mut self, id: CFuncId, cfunc: CFuncDef) {
    assert!(id.0<self.func_id_count,"{:?}",&id);
    self.funcs.insert(id, cfunc);
  }

  pub fn reserve_typedef(&mut self) -> CTypedefId {
    let id = CTypedefId(self.typedef_id_count);
    self.typedef_id_count += 1;
    id
  }
  pub fn add_typedef(&mut self, id: CTypedefId, ctypedef: CTypedef) {
    assert!(id.0<self.typedef_id_count,"{:?}",&id);
    self.typedefs.insert(id, ctypedef);
  }

  pub fn reserve_struct(&mut self) -> CStructId {
    let id = CStructId(self.struct_id_count);
    self.struct_id_count += 1;
    id
  }
  pub fn add_struct(&mut self, id: CStructId, cstruct: CStructDef) {
    assert!(id.0<self.struct_id_count,"{:?}",&id);
    self.structs.insert(id, cstruct);
  }

  pub fn reserve_union(&mut self) -> CUnionId {
    let id = CUnionId(self.union_id_count);
    self.union_id_count += 1;
    id
  }
  pub fn add_union(&mut self, id: CUnionId, cunion: CUnionDef) {
    assert!(id.0<self.union_id_count,"{:?}",&id);
    self.unions.insert(id, cunion);
  }

  pub fn reserve_enum(&mut self) -> CEnumId {
    let id = CEnumId(self.enum_id_count);
    self.enum_id_count += 1;
    id
  }
  pub fn add_enum(&mut self, id: CEnumId, cenum: CEnumDef) {
    assert!(id.0<self.enum_id_count,"{:?}",&id);
    self.enums.insert(id, cenum);
  }

  pub fn reserve_global_var(&mut self) -> CGVarId {
    let id = CGVarId(self.global_var_id_count);
    self.global_var_id_count += 1;
    id
  }
  pub fn add_global_var(&mut self, id: CGVarId, cgvar: CGlobalVar) {
    assert!(id.0<self.global_var_id_count,"{:?}",&id);
    self.global_vars.insert(id, cgvar);
  }
}





impl CExpr {
  pub fn ret_type(&self) -> &CQualType {
    match self {
      CExpr::Literal(qt,..) |
      CExpr::Unary(qt,..) |
      CExpr::Binary(qt,..) |
      CExpr::UnaryOfType(qt,..) |
      CExpr::ImplicitCast(qt,..) |
      CExpr::ExplicitCast(qt,..) |
      CExpr::GlobalVar(qt,..) |
      CExpr::LocalVar(qt,..) |
      CExpr::FuncName(qt,..) |
      CExpr::EnumConst(qt,..) |
      CExpr::Call(qt,..) |
      CExpr::SFieldDot(qt,..) |
      CExpr::UFieldDot(qt,..) |
      CExpr::SFieldArrow(qt,..) |
      CExpr::UFieldArrow(qt,..) |
      CExpr::ArrayIndex(qt,..) |
      CExpr::Ternary(qt,..) |
      CExpr::InitList(qt,..) => qt,
    }
  }
}



/*
 *
 *  Display...
 *
 */
use std::fmt;
use std::collections::BTreeMap;

impl CCompUnit {
  pub fn print_report(&self) {
    println!();
    println!("REPORT");
    println!();
    println!("FUNCS");
    let funcs = BTreeMap::from_iter(self.funcs.iter());
    for (id, fdef) in funcs.iter() {
        println!("  {} {fdef}",id.0);
    }
    println!();
    println!("TYPEDEFS");
    let typedefs = BTreeMap::from_iter(self.typedefs.iter());
    for (id, typedef) in typedefs.iter() {
        println!("  {} {typedef}",id.0);
    }
    println!();
    println!("STRUCTS");
    let structs = BTreeMap::from_iter(self.structs.iter());
    for (id, sdef) in structs.iter() {
        println!("  {} {sdef}",id.0);
    }
    println!();
    println!("UNIONS");
    let unions = BTreeMap::from_iter(self.unions.iter());
    for (id, udef) in unions.iter() {
        println!("  {} {udef}",id.0);
    }
    println!();
    println!("ENUMS");
    let enums = BTreeMap::from_iter(self.enums.iter());
    for (id, edef) in enums.iter() {
        println!("  {} {edef}",id.0);
    }
    println!();
    println!("GLOBAL VARIABLES");
    let gvs = BTreeMap::from_iter(self.global_vars.iter());
    for (id, vdef) in gvs.iter() {
        println!("  {} {vdef:?}",id.0);
    }
  }
}

fn b2b(b : bool) -> char {
  if b {'1'} else {'0'}
}

impl fmt::Display for CFuncDef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let annotation_bits = format!("{}{}{}{}",
      b2b(self.is_global),
      b2b(self.annotations.marked_extern),
      b2b(self.annotations.marked_inline),
      b2b(self.annotations.implicit));
    let ps : Vec<_> = self.params.iter()
      .map(|p| format!("{p}")).collect();
    let body = if self.body.is_none() { "" } else { "\n  {}" };
    let rtyp = &self.typ.rtyp;

    write!(f, "{rtyp} {}({}) //bits:{annotation_bits}{body}",
      self.name, ps.join(", "))
  }
}

impl fmt::Display for CParam {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} {}", self.typ, self.name)
  }
}

impl fmt::Display for CTypedef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let prefix = if self.is_implicit { "implicit " } else { "" };
    write!(f, "{}typedef {} {}", prefix, self.name, self.typ)
  }
}

impl fmt::Display for CStructDef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CStructDef::Decl(nm) => write!(f,"struct {nm}"),
      CStructDef::Defn(cstruct) => write!(f,"{cstruct}"),
    }
  }
}

impl fmt::Display for CStruct {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let nm = match &self.name { None => "", Some(n) => n };
    let fs : Vec<_> = self.fields.iter().map(|f| format!("{f};")).collect();
    let flds = fs.join(" ");
    write!(f,"struct {nm}{{ {flds} }}")
  }
}

impl fmt::Display for CUnionDef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CUnionDef::Decl(nm) => write!(f,"union {nm}"),
      CUnionDef::Defn(cunion) => write!(f,"{cunion}"),
    }
  }
}

impl fmt::Display for CUnion {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      let nm = match &self.name { None => "", Some(n) => n };
      let fs : Vec<_> = self.fields.iter().map(|f| format!("{f};")).collect();
      let flds = fs.join(" ");
      let p = if self.is_packed { "packed " } else { "" };
      write!(f,"{p}union {nm}{{ {flds} }}")
  }
}

impl fmt::Display for CField {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f,"{} {}", self.typ, self.name)
  }
}

impl fmt::Display for CEnumDef {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let nm = match &self.name { None => "", Some(n) => n };
    let cs : Vec<_> = self.consts.iter().map(|c| format!("{c}")).collect();
    let consts = cs.join(", ");
    write!(f,"enum {nm}{{ {consts} }}")
  }
}

impl fmt::Display for CEnumConst {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f,"{}={}", self.name, self.value)
  }
}

impl fmt::Display for CGlobalVar {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CGlobalVar::Decl{ name, typ } => write!(f,"extern {typ} {name};"),
      CGlobalVar::Defn{ name, typ, init, is_extern } => {
        let ext = if *is_extern { "extern " } else { "" };
        match init {
          Some(e) => write!(f,"{ext}{typ} {name} = {e};"),
          None => write!(f,"{ext}{typ} {name};")
        }
      }
    }
  }
}

impl fmt::Display for CExpr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<expr todo>")
  }
}

impl fmt::Display for CQualType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let c = if self.quals.is_const { "const " } else { "" };
    let r = if self.quals.is_restrict { "restrict " } else { "" };
    let v = if self.quals.is_volatile { "volatile " } else { "" };
    write!(f, "{}{}{}{}", c, r, v, *self.typ)
  }
}

impl fmt::Display for CType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CType::Void => write!(f,"void"),
      CType::Prim(pt) => write!(f,"{pt}"),
      CType::Complex(pt) => write!(f,"complex {pt}"),
      CType::Ptr(qt) => write!(f,"({})*",qt),
      CType::ConstSizeArray(t,sz) => write!(f,"({t})[{sz}]"),
      CType::NoSizeArray(t) => write!(f,"({t})[]"),
      CType::VarSizeArray(t,Some(sz)) => write!(f,"({t})[{sz}]"),
      CType::VarSizeArray(t,None) => write!(f,"({t})[*]"),
      CType::Func(ft) => write!(f,"{ft}"),
      CType::BuiltinFn => write!(f,"BuiltinFn"),
      CType::StructDef(id) => write!(f, "struct {}", id.0),
      CType::UnionDef(id) => write!(f, "union {}", id.0),
      CType::EnumDef(id) => write!(f, "enum {}", id.0),
      CType::Typedef(id) => write!(f, "typedef-{}", id.0),

      CType::Decayed(t) => write!(f, "decayed({t})"),

      CType::Struct(cstruct) => write!(f, "{cstruct}"),
      CType::Union(cunion) => write!(f, "{cunion}"),
      CType::Enum(cenum) => write!(f, "{cenum}"),

      CType::Unimplemented(s) => write!(f,"UNIMPLEMENTED({})",s),
    }
  }
}

impl fmt::Display for CFuncType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let va = if self.annotations.is_var_arg { "vararg " } else { "" };
    let nr = if self.annotations.is_noreturn { "noreturn " } else { "" };
    let p = if self.annotations.has_prototype { "withproto " } else { "" };
    let ps : Vec<_> = self.ptyps.iter().map(|qt| format!("{qt}")).collect();
    let pstr = ps.join(", ");
    write!(f,"({va}{nr}{p}{}({pstr}))", self.rtyp)
  }
}

impl fmt::Display for CPrimType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      CPrimType::Bool => { write!(f, "bool") }
      CPrimType::Char => { write!(f, "char") }
      CPrimType::SChar => { write!(f, "schar") }
      CPrimType::Short => { write!(f, "short") }
      CPrimType::Int => { write!(f, "int") }
      CPrimType::Long => { write!(f, "long") }
      CPrimType::LongLong => { write!(f, "longlong") }
      CPrimType::UChar => { write!(f, "uchar") }
      CPrimType::UShort => { write!(f, "ushort") }
      CPrimType::UInt => { write!(f, "uint") }
      CPrimType::ULong => { write!(f, "ulong") }
      CPrimType::ULongLong => { write!(f, "ulonglong") }
      CPrimType::Int128 => { write!(f, "int128") }
      CPrimType::UInt128 => { write!(f, "uint128") }
      CPrimType::Float => { write!(f, "float") }
      CPrimType::Double => { write!(f, "double") }
      CPrimType::LongDouble => { write!(f, "longdouble") }
      CPrimType::Half => { write!(f, "half") }
      CPrimType::BFloat16 => { write!(f, "bf16") }
    }
  }
}

impl fmt::Display for IntLiteralValue {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      IntLiteralValue::ULit(u) => write!(f,"{u}"),
      IntLiteralValue::ILit(i) => write!(f,"{i}"),
    }
  }
}

