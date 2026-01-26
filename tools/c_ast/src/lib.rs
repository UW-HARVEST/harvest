use clang_ast::Node;
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

#[derive(Serialize, Deserialize, Debug)]
pub struct QualType {
    #[serde(rename = "desugaredQualType")]
    pub desugared_qual_type: Option<String>,
    #[serde(rename = "qualType")]
    pub qual_type: String,
    #[serde(rename = "typeAliasDecId")]
    pub type_alias_dec_id: Option<clang_ast::Id>,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Clang {
    TranslationUnitDecl,
    TypedefDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: String,
        #[serde(rename = "type")]
        qtype: QualType,
        annotation: Option<String>,
    },
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
    ParmVarDecl {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        name: Option<String>,
        #[serde(rename = "type")]
        qtype: QualType,
        annotation: Option<String>,
    },
    CompoundStmt {
        loc: Option<clang_ast::SourceLocation>,
        range: Option<clang_ast::SourceRange>,
        annotation: Option<String>,
    },
    Other {
        kind: Option<String>,
        annotation: Option<String>,
    },
}

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

#[derive(Serialize)]
pub struct ClangAst {
    pub asts: HashMap<String, Node<Clang>>,
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
        // Expect exactly one input: the RawSource representation
        let source_id = inputs
            .into_iter()
            .next()
            .ok_or("parse_to_ast requires exactly one input")?;

        let rs = context
            .ir_snapshot
            .get::<full_source::RawSource>(source_id)
            .ok_or("Expected RawSource representation")?;

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
            let file = cc.file.replace(src_dir_prefix.as_str(), "");
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
            source_representation: source_id,
            asts,
        }))
    }
}
