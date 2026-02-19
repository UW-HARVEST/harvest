use clang_ast::Node;
use full_source::RawSource;
use harvest_core::{
    Id, Representation,
    tools::{RunContext, Tool},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    process::{Command, Stdio},
};
use tracing::info;

/// Represents a (possibly) qualified type in the Clang AST, such as `int`, `const int`, or `const volatile int`.
/// Clang Docs on QualType: https://clang.llvm.org/doxygen/classclang_1_1QualType.html
#[derive(Serialize, Deserialize, Debug)]
pub struct QualType {
    /// String representation of the desugared type, i.e., it will have `typedefs` and `typeofs` resolved.
    #[serde(rename = "desugaredQualType")]
    pub desugared_qual_type: Option<String>,
    /// String representation of the type as written in the source code, i.e., it may include `typedefs` and `typeofs`.
    #[serde(rename = "qualType")]
    pub qual_type: String,
    /// The ID of the type alias declaration, if this type is a typedef.
    #[serde(rename = "typeAliasDecId")]
    pub type_alias_dec_id: Option<clang_ast::Id>,
}

/// Represents a node in the Clang AST.
/// For the sake of simplicity, it only encodes a subset of the Clang AST nodes that are relevant for our analysis.
#[derive(Serialize, Deserialize, Debug)]
pub enum Clang {
    TranslationUnitDecl,
    /// Represents a typedef declaration in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1TypedefDecl.html
    TypedefDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: String,
        #[serde(rename = "type")]
        qtype: QualType,
        annotation: Option<String>,
    },
    /// Represents a function declaration in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1FunctionDecl.html
    FunctionDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: String,
        #[serde(rename = "storageClass")]
        storage_class: Option<String>,
        #[serde(rename = "type")]
        qtype: QualType,
        annotation: Option<FunctionAnnotation>,
    },
    /// Represents a record (struct/union) declaration in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1RecordDecl.html
    RecordDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: Option<String>,
        #[serde(rename = "tagUsed")]
        tag_used: Option<String>,
        annotation: Option<String>,
    },
    /// Represents an enum declaration in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1EnumDecl.html
    EnumDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: Option<String>,
        annotation: Option<String>,
    },
    /// Represents a variable declaration in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1VarDecl.html
    VarDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: String,
        #[serde(rename = "type")]
        qtype: QualType,
        #[serde(rename = "storageClass")]
        storage_class: Option<String>,
        annotation: Option<String>,
    },
    /// Represents a parameter variable declaration in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1ParmVarDecl.html
    ParmVarDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: Option<String>,
        #[serde(rename = "type")]
        qtype: QualType,
        annotation: Option<String>,
    },
    /// Represents a compound statement in the Clang AST.
    /// Clang Docs: https://clang.llvm.org/doxygen/classclang_1_1CompoundStmt.html
    CompoundStmt {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        annotation: Option<String>,
    },
    /// Every other node (not relevant to our analysis at the moment)
    Other {
        kind: Option<String>,
        annotation: Option<String>,
    },
}

impl Clang {
    /// Returns the source location of this AST node, if available.
    ///
    /// # Returns
    /// - `Some(&SourceLocation)` if the node has a location field
    /// - `None` if the node doesn't have a location or if the location field is None
    pub fn loc(&self) -> Option<&clang_ast::SourceLocation> {
        match self {
            Clang::TranslationUnitDecl => None,
            Clang::TypedefDecl { loc, .. }
            | Clang::FunctionDecl { loc, .. }
            | Clang::RecordDecl { loc, .. }
            | Clang::VarDecl { loc, .. }
            | Clang::EnumDecl { loc, .. }
            | Clang::ParmVarDecl { loc, .. }
            | Clang::CompoundStmt { loc, .. } => loc.as_ref(),
            Clang::Other { .. } => None,
        }
    }

    /// Returns the source range of this AST node, if available.
    ///
    /// # Returns
    /// - `Some(&SourceRange)` if the node has a range field
    /// - `None` if the node doesn't have a range or if the range field is None
    pub fn range(&self) -> Option<&clang_ast::SourceRange> {
        match self {
            Clang::TranslationUnitDecl => None,
            Clang::TypedefDecl { range, .. }
            | Clang::FunctionDecl { range, .. }
            | Clang::RecordDecl { range, .. }
            | Clang::VarDecl { range, .. }
            | Clang::ParmVarDecl { range, .. }
            | Clang::EnumDecl { range, .. }
            | Clang::CompoundStmt { range, .. } => range.as_ref(),
            Clang::Other { .. } => None,
        }
    }

    /// Returns the name of this declaration, if available.
    ///
    /// # Returns
    /// - `Some(String)` if the declaration has a name field and it is populated
    /// - `None` if the declaration doesn't have a name or if the name field is None
    pub fn name(&self) -> Option<String> {
        match self {
            Clang::TypedefDecl { name, .. } => Some(name.clone()),
            Clang::FunctionDecl { name, .. } => Some(name.clone()),
            Clang::RecordDecl { name, .. } => name.clone(),
            Clang::VarDecl { name, .. } => Some(name.clone()),
            Clang::EnumDecl { name, .. } => name.clone(),
            _ => None,
        }
    }
}

/// Our annotations for functions in the Clang AST.
/// Will expand as Harvest's analyses get more sophisticated.
#[non_exhaustive]
#[derive(Serialize, Deserialize, Debug)]
pub enum FunctionAnnotation {
    Entry,
    Static,
}

fn annotate_ast(ast: &mut Node<Clang>) {
    if let Clang::FunctionDecl {
        name,
        storage_class,
        annotation,
        ..
    } = &mut ast.kind
    {
        match storage_class.as_ref().map(|s| s.as_str()) {
            None if name == "main" => *annotation = Some(FunctionAnnotation::Entry),
            Some("static") => *annotation = Some(FunctionAnnotation::Static),
            _ => {}
        }
    }

    for node in ast.inner.iter_mut() {
        annotate_ast(node);
    }
}

/// Represents a Clang AST for a set of source files.
#[derive(Serialize)]
pub struct ClangAst {
    /// Maps file paths to the root node of the Clang AST for that file.
    pub asts: HashMap<String, Node<Clang>>,
    /// The ID of the source representation from which this AST was generated.
    pub source_representation: Id,
}

impl std::fmt::Display for ClangAst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "C ASTs for {:?}", self.asts.keys())
    }
}

impl Representation for ClangAst {
    fn name(&self) -> &'static str {
        "clang_ast"
    }

    fn materialize(&self, path: &std::path::Path) -> std::io::Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer(file, self).map_err(Into::into)
    }
}

pub struct ParseToAst;

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

        let working_dir = tempfile::TempDir::new()?;

        let src_dir = tempfile::TempDir::new()?;
        rs.dir.materialize(src_dir.path())?;
        let src_dir_prefix = format!("{}/", src_dir.path().to_str().unwrap());

        Command::new("cmake")
            .args(["-DCMAKE_EXPORT_COMPILE_COMMANDS=1"])
            .arg("-S")
            .arg(src_dir.path())
            .arg("-B")
            .arg(working_dir.path())
            .output()?;

        #[derive(Deserialize, Debug)]
        struct CompileCommand {
            command: String,
            file: String,
        }
        let ccs: Vec<CompileCommand> = serde_json::de::from_reader(File::open(
            working_dir.path().join("compile_commands.json"),
        )?)?;

        let mut asts: HashMap<String, clang_ast::Node<Clang>> = Default::default();

        info!(
            "Parsing {} files: {}",
            ccs.len(),
            ccs.iter()
                .map(|cc| cc.file.as_str())
                .collect::<Vec<&str>>()
                .join(", ")
        );

        for cc in ccs {
            let file = cc
                .file
                .strip_prefix(src_dir_prefix.as_str())
                .unwrap_or(&cc.file)
                .to_string();
            let includes = cc
                .command
                .split(" ")
                .filter(|p| p.starts_with("-I"))
                .map(|p| p.replace(src_dir_prefix.as_str(), ""));

            let mut clang_cmd = Command::new("clang");
            let clang_cmd = clang_cmd
                .current_dir(src_dir.path())
                .args(["-Xclang", "-ast-dump=json", "-fsyntax-only"])
                .args(includes)
                .arg(&file)
                .stderr(Stdio::null())
                .stdout(Stdio::piped());
            let mut clang = clang_cmd.spawn()?;
            let mut ast: clang_ast::Node<Clang> =
                serde_json::from_reader(clang.stdout.take().unwrap())?;
            clang.wait()?;
            annotate_ast(&mut ast);
            info!("Parsed {file}");
            asts.insert(file, ast);
        }

        Ok(Box::new(ClangAst {
            source_representation: id,
            asts,
        }))
    }
}
