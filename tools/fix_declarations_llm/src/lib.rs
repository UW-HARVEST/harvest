//! `FixDeclarationsLlm`: calls the LLM to repair declarations that have compiler errors,
//! producing an updated `SplitPackage` with fixed declarations and a recomputed line index.

use diagnostic_attributor::DeclarationDiagnostics;
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::{ChatMessage, HarvestLLM, LLMConfig};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::Deserialize;
use serde_json::Value;
use split_and_format::{SplitPackage, unparse_item};
use std::collections::HashMap;
use tracing::{info, warn};

/// Configuration read from `[tools.fix_declarations_llm]`.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub llm: LLMConfig,

    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    fn validate(&self) {
        unknown_field_warning("tools.fix_declarations_llm", &self.unknown);
    }
}

// LLM wrapper

#[derive(Debug, Deserialize)]
struct FixResult {
    fixed_code: String,
}

struct FixLlm {
    llm: HarvestLLM,
}

impl FixLlm {
    fn new(config: &LLMConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let system_prompt = include_str!("prompts/fix/system_prompt.txt");
        let schema = include_str!("prompts/fix/structured_schema.json");
        let llm = HarvestLLM::build(config, Some(schema), system_prompt)?;
        Ok(FixLlm { llm })
    }

    fn fix_declaration(
        &self,
        decl_source: &str,
        errors_text: &str,
        context: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let prompt = include_str!("prompts/fix/user_prompt.txt")
            .replace("{context}", context)
            .replace("{errors}", errors_text)
            .replace("{declaration}", decl_source);

        let messages = vec![ChatMessage::user().content(&prompt).build()];
        let (response, _usage) = self.llm.invoke(&messages)?;
        let result: FixResult = serde_json::from_str(&response).map_err(|e| {
            format!("Failed to parse fix LLM response as JSON: {e}\nResponse: {response}")
        })?;

        Ok(result.fixed_code.trim_end_matches('\n').to_string())
    }
}

// Stub helpers (for interface context)

fn stub_item(item: syn::Item) -> syn::Item {
    match item {
        syn::Item::Fn(mut f) => {
            f.block = Box::new(syn::parse_quote!({ todo!() }));
            syn::Item::Fn(f)
        }
        syn::Item::Impl(mut impl_block) => {
            impl_block.items = impl_block
                .items
                .into_iter()
                .map(|impl_item| match impl_item {
                    syn::ImplItem::Fn(mut method) => {
                        method.block = syn::parse_quote!({ todo!() });
                        syn::ImplItem::Fn(method)
                    }
                    other => other,
                })
                .collect();
            syn::Item::Impl(impl_block)
        }
        other => other,
    }
}

fn stub_declaration(source: &str) -> String {
    let Ok(file) = syn::parse_file(source) else {
        return source.trim_end_matches('\n').to_string();
    };
    file.items
        .into_iter()
        .map(|item| {
            let stubbed = stub_item(item);
            unparse_item(&stubbed).trim_end_matches('\n').to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build an interface context string: all declarations with function bodies replaced
/// by `{ todo!() }`. Used as reference context for the LLM in each fix call.
fn build_interface_context(declarations: &[String]) -> String {
    declarations
        .iter()
        .map(|d| stub_declaration(d))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Calls the LLM to fix each declaration that has compiler errors, and returns a new
/// `SplitPackage` with updated declarations and a freshly computed line index.
pub struct FixDeclarationsLlm;

impl Tool for FixDeclarationsLlm {
    fn name(&self) -> &'static str {
        "fix_declarations_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(context.config.tools.get("fix_declarations_llm").ok_or(
            "No fix_declarations_llm config found in config.toml. \
                     Please add a [tools.fix_declarations_llm] section.",
        )?)?;
        config.validate();

        let split_pkg = context
            .ir_snapshot
            .get::<SplitPackage>(inputs[0])
            .ok_or("FixDeclarationsLlm: no SplitPackage found in IR")?;

        let decl_diags = context
            .ir_snapshot
            .get::<DeclarationDiagnostics>(inputs[1])
            .ok_or("FixDeclarationsLlm: no DeclarationDiagnostics found in IR")?;

        // Nothing to fix. Return a clone of the input split package.
        if !decl_diags.has_errors {
            return Ok(Box::new(SplitPackage::from_declarations(
                split_pkg.declarations.clone(),
                split_pkg.cargo_toml.clone(),
                split_pkg.source_file_name.clone(),
            )));
        }

        let fix_llm = FixLlm::new(&config.llm)?;

        // Build interface context once: all declarations stubbed (try to trigger LLM prefix caching).
        let interface_ctx = build_interface_context(&split_pkg.declarations);

        let mut declarations = split_pkg.declarations.clone();
        let mut fixed_count = 0usize;

        for (&decl_idx, error_texts) in &decl_diags.decl_errors {
            let decl_source = declarations[decl_idx].clone();
            let errors_text = error_texts.join("\n\n");

            match fix_llm.fix_declaration(&decl_source, &errors_text, &interface_ctx) {
                Ok(fixed) if !fixed.is_empty() => {
                    declarations[decl_idx] = fixed;
                    fixed_count += 1;
                }
                Ok(_) => {
                    warn!(
                        "FixDeclarationsLlm: LLM returned empty response for declaration {}",
                        decl_idx
                    );
                }
                Err(e) => {
                    warn!(
                        "FixDeclarationsLlm: LLM fix failed for declaration {}: {}",
                        decl_idx, e
                    );
                }
            }
        }

        info!(
            "FixDeclarationsLlm: fixed {}/{} declarations",
            fixed_count,
            decl_diags.decl_errors.len()
        );

        Ok(Box::new(SplitPackage::from_declarations(
            declarations,
            split_pkg.cargo_toml.clone(),
            split_pkg.source_file_name.clone(),
        )))
    }
}
