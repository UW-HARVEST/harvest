//! Modular translation for C->Rust. Decomposes a C project AST into its top-level modules and translates them one-by-one using an LLM.

use c_ast::ClangAst;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use identify_project_kind::ProjectKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {}

/// The main tool struct for modular translation.
pub struct ModularTranslationLlm;

impl Tool for ModularTranslationLlm {
    fn name(&self) -> &'static str {
        "modular_translation_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let _clang_ast = context
            .ir_snapshot
            .get::<ClangAst>(inputs[0])
            .ok_or("No ClangAst representation found in IR")?;
        let _project_kind = context
            .ir_snapshot
            .get::<ProjectKind>(inputs[1])
            .ok_or("No ProjectKind representation found in IR")?;
        // TODO: Implement the run method
        // TODO: Implement the run method
        // Should return a CargoPackage representation
        Err("modular_translation_llm not yet implemented".into())
    }
}
