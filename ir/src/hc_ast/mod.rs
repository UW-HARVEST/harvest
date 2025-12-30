/*
 *  This file contains the data structures for
 *  HARVEST-IR's version of C ASTs.
 *
 *  Original Intentions when creating this file:
 *  - Code for marshalling data into and out of this representation
 *    should be located in other modules as much as possible
 *  - Code for validating invariants of the IR belongs here,
 *    rather than in a supporting file
 *
 *  Initial conventions on IR encoding in Rust have mostly been inherited
 *  from C2Rust.  Thus such choices were made for expediency, and should be
 *  revisited in the future by someone with a better sense of the best
 *  way to encode IRs in Rust.
 *
 *  Many pieces of code are taken verbatim or with modification from
 *  Galois and Immunant's C2Rust code base, as permitted under the
 *  MIT License for that software.
 *
 */

pub mod support;
use std::collections::{HashSet, HashMap};

#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Copy, Clone)]
pub struct CFuncId(pub u64);

#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Copy, Clone)]
pub struct CTypedefId(pub u64);

#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Copy, Clone)]
pub struct CStructId(pub u64);

#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Copy, Clone)]
pub struct CUnionId(pub u64);

#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Copy, Clone)]
pub struct CEnumId(pub u64);

#[derive(Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Copy, Clone)]
pub struct CGVarId(pub u64);

#[derive(Debug)]
pub struct CCompUnit {
  func_id_count: u64,
  pub funcs: HashMap<CFuncId, CFuncDef>,

  typedef_id_count: u64,
  pub typedefs: HashMap<CTypedefId, CTypedef>,

  struct_id_count: u64,
  pub structs: HashMap<CStructId, CStructDef>,

  union_id_count: u64,
  pub unions: HashMap<CUnionId, CUnionDef>,

  enum_id_count: u64,
  pub enums: HashMap<CEnumId, CEnumDef>,

  global_var_id_count : u64,
  pub global_vars: HashMap<CGVarId, CGlobalVar>,
}

#[derive(Debug)]
pub struct CFuncDef {
  pub name : String,
  pub typ : CFuncType,
  pub params : Vec<CParam>,
  pub body : Option<Box<CStmt>>,
  pub is_global : bool,
  pub annotations : CFuncAnnotations,
  // TODO: Incorporate attributes
}

impl CFuncDef {
  pub fn is_decl(&self) -> bool { self.body.is_none() }
  pub fn is_defn(&self) -> bool { self.body.is_some() }
}

#[derive(Debug)]
pub struct CFuncAnnotations {
  pub marked_extern : bool,
  pub marked_inline : bool,
  pub implicit : bool, // only applicable to declarations
                      // means that no declaration was given, despite the
                      // function being used
}

#[derive(Debug)]
pub struct CParam {
  pub name: String,
  pub typ: CQualType,
  //pub annotations: CParamAnnotations,
  // TODO: Incorporate attributes
}

#[derive(Debug)]
pub struct CParamAnnotations {
  pub has_static_duration : bool,
  pub has_thread_duration : bool,
  pub is_externally_visible : bool,
  pub is_defn: bool,
}

#[derive(Debug)]
pub struct CTypedef {
  pub name: String,
  pub typ: CQualType,
  pub is_implicit: bool, // implicit type-defs are all built-in?
}

#[derive(Debug)]
pub enum CStructDef {
  Decl(String),
  Defn(CStruct),
}

#[derive(Debug)]
pub struct CStruct {
  pub name: Option<String>,
  pub fields: Vec<CField>,
  pub align: CStructAlignment,
  //is_packed: bool,
  //manual_alignment: Option<u64>,
  //max_field_alignment: Option<u64>,
  //platform_byte_size: u64,
  //platform_alignment: u64,
}

#[derive(Debug)]
pub struct CStructAlignment {
  pub is_packed: bool,
  pub manual_alignment: Option<u64>,
  pub max_field_alignment: Option<u64>,
  pub platform_byte_size: u64,
  pub platform_alignment: u64,
}

#[derive(Debug)]
pub enum CUnionDef {
  Decl(String),
  Defn(CUnion),
}

#[derive(Debug)]
pub struct CUnion {
  pub name: Option<String>,
  pub fields: Vec<CField>,
  // is_packed has small effects, e.g. alignment
  // mainly useful for inferring programmer intent
  pub is_packed: bool,
}

#[derive(Debug)]
pub struct CField {
  pub name: String,
  pub typ: CQualType,
  //bitfield_width: Option<u64>,
  //platform_bit_offset: u64,
  //platform_type_bitwidth: u64,
}

#[derive(Debug)]
pub struct CEnumDef {
  pub name: Option<String>,
  pub consts: Vec<CEnumConst>,
  //value_type: Option<CQualType>, // why a qualified type...???
}

#[derive(Debug)]
pub struct CEnumConst {
  pub name: String,
  pub value: IntLiteralValue,
}

#[derive(Debug)]
pub enum CGlobalVar {
  Decl{
    name: String,
    typ: CQualType,
  },
  Defn{
    name: String,
    typ: CQualType,
    init: Option<Box<CExpr>>,
    is_extern: bool,
  },
}




#[derive(Debug)]
pub enum CStmt {
  Block(Vec<Box<CStmt>>),
  Noop,

  Expr(Box<CExpr>),

  If(Box<CExpr>, Box<CStmt>, Option<Box<CStmt>>),

  While(Box<CExpr>, Box<CStmt>),
  DoWhile(Box<CStmt>, Box<CExpr>),
  ForLoop(Option<Box<CStmt>>, Option<Box<CExpr>>, Option<Box<CExpr>>,
          Box<CStmt>),
  Break,
  Continue,

  Return(Option<Box<CExpr>>),

  // TODO: Switch, Labels & Gotos, Decls (Var?), inline assembly, attributed
}


#[derive(Debug)]
// first argument is the type of the expression sub-tree
pub enum CExpr {
  Literal(CQualType, CLiteral),
  Unary(CQualType, UnaryOp, Box<CExpr>, LVal),
  Binary(CQualType, BinOp, Box<CExpr>, Box<CExpr>,
         Option<CQualType>, Option<CQualType>),
  UnaryOfType(CQualType, UOfTypeOp, Option<Box<CExpr>>, CQualType),
  // "unary of type" operators can be applied to either types
  // or to expressions; in the latter case, the expression is kept
  // in the AST as the Option<...> field above.  The last type above is the
  // type to which the operator is implicitly or explicitly being applied

  // two additional fields on C2Rust:  CastKind, Option<FIELD>, 
  // TODO: Check their values and try to figure out whether
  //       we should keep or discard that data
  ImplicitCast(CQualType, Box<CExpr>, LVal),
  ExplicitCast(CQualType, Box<CExpr>, LVal),

  // names referring to different kinds of declarations...
  // TODO: Decide how to handle references...
  GlobalVar(CQualType, CGVarId, String, LVal),
  // TODO: add ID to Local Vars?
  LocalVar(CQualType, String, LVal),
  FuncName(CQualType, CFuncId, String, LVal),
  // String is the constant name, which enum we are in can be
  // determined by interrogating the expression's type
  EnumConst(CQualType, String, IntLiteralValue, LVal),

  // `func(args)` is (rtyp, func, args)
  Call(CQualType, Box<CExpr>, Vec<Box<CExpr>>),
  // `obj.field`, or `obj->field` where `obj` is a struct or union
  // is given as (rtyp, obj, field_num, lval)
  SFieldDot(CQualType, Box<CExpr>, u32, LVal),
  SFieldArrow(CQualType, Box<CExpr>, u32, LVal),
  UFieldDot(CQualType, Box<CExpr>, u32, LVal),
  UFieldArrow(CQualType, Box<CExpr>, u32, LVal),
  // `obj[index]` is (rtyp, obj, index, lval)
  ArrayIndex(CQualType, Box<CExpr>, Box<CExpr>, LVal),
  // `(cond)? tcase : fcase` is (rtyp, cond, tcase, fcase)
  Ternary(CQualType, Box<CExpr>, Box<CExpr>, Box<CExpr>),
  // `{e0, e1, ...}` is (rtyp, entries)
  InitList(CQualType, Vec<Box<InitListEntry>>),
}

// e.g. `[index] = iexpr` or
//      `.field = iexpr` or
// combinations thereof, such as `[idx].f = iexpr`,
//    `[idx1][idx2] = ...`, `.f1.f2 = ...`
// Val() is just the `= iexpr` part
// The other two cases are the different designations.
// Designations are wrapped s.t. leftmost is outermost in the AST
#[derive(Debug)]
pub enum InitListEntry {
  Val(Box<CExpr>),
  // an explicitly designated index
  IndexDesignation(u64, Box<InitListEntry>),
  // (field index in struct, remainder of entry)
  StructDesignation(u32, Box<InitListEntry>),
  // (field index in union, remainder of entry)
  UnionDesignation(u32, Box<InitListEntry>),
}

pub type LVal = bool; // true means this expression is an LValue

// from C2Rust
#[derive(Debug)]
pub enum UnaryOp {
    AddressOf,  // &x
    Deref,      // *x
    Plus,       // +x
    PostInc,    // x++
    PreInc,     // ++x
    Minus,      // -x
    PostDec,    // x--
    PreDec,     // --x
    BitNot,     // ~x
    Not,        // !x
    Real,       // [GNU C] __real x
    Imag,       // [GNU C] __imag x
    Extension,  // [GNU C] __extension__ x
}

// from C2Rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Mult,   // *
    Div,    // /
    Mod,    // %
    Add,    // +
    Sub,    // -
    ShiftL, // <<
    ShiftR, // >>
    Lt,     // <
    Gt,     // >
    Le,     // <=
    Ge,     // >=
    Eq,     // ==
    Neq,    // !=
    BitAnd, // &
    BitXor, // ^
    BitOr,  // |
    And,    // &&
    Or,     // ||

    AssignAdd,    // +=
    AssignSub,    // -=
    AssignMult,   // *=
    AssignDiv,    // /=
    AssignMod,    // %=
    AssignBitXor, // ^=
    AssignShiftL, // <<=
    AssignShiftR, // >>=
    AssignBitOr,  // |=
    AssignBitAnd, // &=

    Assign,       // =
    Comma,        // ,
}

#[derive(Debug, Clone, Copy)]
pub enum UOfTypeOp {
    SizeOf,
    AlignOf,
    //PreferredAlignOf,
}


// TO CHECK:
// Should A function with no parameters have one Void parameter or not???
#[derive(Debug)]
pub struct CFuncType {
  pub rtyp: CQualType,
  pub ptyps: Vec<CQualType>,
  pub annotations: CFuncTypeAnnotations,
}

#[derive(Debug)]
pub struct CFuncTypeAnnotations {
  pub is_var_arg: bool,
  // note that noreturn does not mean `Void` return type.
  // it is an annotation that means the function is never expected
  // to return control; e.g. because it is an unbounded continuation.
  pub is_noreturn: bool,
  pub has_prototype: bool,
}

#[derive(Debug)]
pub struct CQualType {
  pub typ: Box<CType>,
  pub quals: CTypeQualifiers,
}

#[derive(Debug)]
pub struct CTypeQualifiers {
  pub is_const: bool,
  pub is_restrict: bool,
  pub is_volatile: bool,
}

#[derive(Debug)]
pub enum CType {
  Void,

  Prim(CPrimType),

  Complex(CPrimType),

  Ptr(CQualType),
  //Ref(CQualType), // only in C++?

  // constant size array
  ConstSizeArray(Box<CType>, usize),
  // array type with no size given
  NoSizeArray(Box<CType>),
  // array with expression size given
  // None means that '*' was used as the expression
  VarSizeArray(Box<CType>, Option<Box<CExpr>>),

  Func(CFuncType),
  BuiltinFn,
  // TODO: is the above the right way to handle built-ins?
  // Do we have/need a list of all possible built-in functions in C?

  // these represent types specified via top-level declarations
  StructDef(CStructId),
  UnionDef(CUnionId),
  EnumDef(CEnumId),
  Typedef(CTypedefId),

  // these represent types specified inline (hence no recursion)
  Struct(CStruct),
  Union(CUnion),
  Enum(CEnumDef),

  Unimplemented(String),

  // Not quite sure how to interpret these...
  //Elaborated(Box<CType>),
  Decayed(Box<CType>),
}

#[derive(Debug)]
pub enum CPrimType {
  Bool,
  Char,

  // signed
  SChar,      // i8
  Short,      // i16
  Int,        // i16 or i32 (probably latter)
  Long,       // i32 or i64
  LongLong,   // i64 or greater

  // unsigned
  UChar,      // u8
  UShort,     // u16
  UInt,       // u16 or u32 (probably latter)
  ULong,      // u32 or u64
  ULongLong,  // u64 or greater

  // Clang specific types
  Int128,
  UInt128,

  // floating point types
  Float,      // 32-bit
  Double,     // 64-bit
  LongDouble, // 80 or 128 bit -- many possible semantics
  // maybe supported?
  Half,       // 16-bit
  BFloat16,   // alternate 16-bit floating point
}

#[derive(Debug)]
pub enum IntLiteralValue {
  ULit(u64),
  ILit(i64),
}

#[derive(Debug)]
pub enum CIntBase { Dec, Hex, Oct }

#[derive(Debug)]
pub enum CLiteral {
  // I(nt), C(har), F(loat), S(tring)
  I(u64, CIntBase),
  C(u64),
  F(f64, String),
  S(Vec<u8>, u8), // bytes and "unit byte width" (from llvm)
}


// THE following are random notes; should probably be cleaned up.

// Complex uses CTypeId
  // this has to be a primitive scalar type
// ConstantArray uses CTypeId, as does Incomplete Array and Variable Array
  // all of these may broadly use 
/*
  From the C99 Standard: A typeof construct can be used anywhere
  a typedef name can be used. For example, you can use it in a declaration,
  in a cast, or inside of sizeof or typeof.
*/

/*
 *
 *  Constructors
 *
 */

impl CCompUnit {
  pub fn new() -> Self {
    Self{
      func_id_count: 1, // is a uid assignment counter, not necc. count
      funcs: HashMap::new(),
      typedef_id_count: 1, // is a uid assignment counter, not necc. count
      typedefs: HashMap::new(),
      struct_id_count: 1, // is a uid assignment counter, not necc. count
      structs: HashMap::new(),
      union_id_count: 1, // is a uid assignment counter, not necc. count
      unions: HashMap::new(),
      enum_id_count: 1, // is a uid assignment counter, not necc. count
      enums: HashMap::new(),
      global_var_id_count: 1, // is a uid assignment counter, not necc. count
      global_vars: HashMap::new(),
    }
  }
}

impl CFuncAnnotations {
  pub fn new(ext: bool, inline: bool, implicit: bool) -> Self {
    Self{ marked_extern: ext, marked_inline: inline, implicit: implicit }
  }
}

impl CFuncTypeAnnotations {
  pub fn new(varg: bool, noret: bool, has_proto: bool) -> Self {
    Self{ is_var_arg: varg, is_noreturn: noret, has_prototype: has_proto }
  }
}

impl CTypeQualifiers {
  pub fn new(is_const: bool, restrict: bool, volatile: bool) -> Self {
    Self{ is_const: is_const, is_restrict: restrict, is_volatile: volatile }
  }
}

/*
 *
 *  Data Structure Validation / Invariants
 *
 */

impl CCompUnit {
  pub fn validate(&self) {
    self.validate_fn_names_disjoint();
    self.validate_struct_names_disjoint();
    self.validate_globvar_names_disjoint();

    for fd in self.funcs.values() {
      self.validate_cfuncdef(fd);
    }
  }

  fn validate_fn_names_disjoint(&self) {
    let mut fn_names = HashSet::new();
    for fdef in self.funcs.values() {
      if fn_names.contains(&fdef.name) {
          panic!("Found duplicate function name: {}", &fdef.name);
      }
      fn_names.insert(fdef.name.clone());
    }
  }

  fn validate_struct_names_disjoint(&self) {
    let mut s_names = HashSet::new();
    for sd in self.structs.values() {
      match sd {
        CStructDef::Decl(name) |
        CStructDef::Defn(CStruct{ name: Some(name), .. }) => {
          if s_names.contains(name) {
            panic!("Found duplicate struct name: {}", name);
          }
          s_names.insert(name.clone());
        }
        _ => {}, // no name struct
      }
    }
  }

  fn validate_globvar_names_disjoint(&self) {
    let mut gv_names = HashSet::new();
    for gv in self.global_vars.values() {
      match gv {
        CGlobalVar::Decl{ name, .. } |
        CGlobalVar::Defn{ name, .. } => {
          if gv_names.contains(name) {
            panic!("Found duplicate global variable name: {}", name);
          }
          gv_names.insert(name.clone());
        }
      }
    }
  }

  fn validate_cfuncdef(&self, fd: &CFuncDef) {
    // TODO: check type correspondence
    //       but for now at least checking if number of params is the same
    assert!(fd.typ.ptyps.len() == fd.params.len());
  }
}





