mod generators;

use full_source::RawSource;
use generators::{StructMap, TestVector, generate_test_vectors};
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::{HarvestLLM, LLMConfig, LLMUsageTotals, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

const SCHEMA_API: &str = include_str!("structured_schema_api.json");
const PROMPT_API: &str = include_str!("system_prompt_api.txt");

const TMPL_HEADER: &str = include_str!("templates/header.c");
const TMPL_PROLOGUE: &str = include_str!("templates/test_prologue.c");
const TMPL_BODY_SCALAR: &str = include_str!("templates/body_scalar.c");
const TMPL_BODY_VOID: &str = include_str!("templates/body_void.c");
const TMPL_BODY_STRING: &str = include_str!("templates/body_string.c");
const TMPL_BODY_WARN: &str = include_str!("templates/body_warn.c");
const TMPL_MAIN: &str = include_str!("templates/main.c");

/// A generated C differential test suite that loads both the C and Rust shared libraries via
/// dlopen and compares their outputs on identical inputs.
pub struct DiffTestSuite {
    pub source: String,
}

impl std::fmt::Display for DiffTestSuite {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.source)
    }
}

impl Representation for DiffTestSuite {
    fn name(&self) -> &'static str {
        "diff_test_suite"
    }
}

pub struct GenerateDiffTestSuite;

// ── Internal types ────────────────────────────────────────────────────────────

pub(crate) struct FnSig {
    pub(crate) return_type: String,
    pub(crate) param_types: Vec<String>,
}

// ── API extraction ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ApiFunction {
    name: String,
    return_type: String,
    param_types: Vec<String>,
}

#[derive(Deserialize)]
struct ApiField {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Deserialize)]
struct ApiStruct {
    name: String,
    fields: Vec<ApiField>,
}

#[derive(Deserialize)]
struct ApiResponse {
    functions: Vec<ApiFunction>,
    structs: Vec<ApiStruct>,
}

fn extract_c_api(
    files: &[(PathBuf, &[u8])],
    config: &Config,
) -> Result<(HashMap<String, FnSig>, StructMap), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct InputFile {
        path: String,
        contents: String,
    }

    #[derive(Serialize)]
    struct RequestBody {
        files: Vec<InputFile>,
    }

    let request_files = files
        .iter()
        .map(|(path, contents)| InputFile {
            path: path.to_string_lossy().into_owned(),
            contents: String::from_utf8_lossy(contents).into_owned(),
        })
        .collect();

    let llm = HarvestLLM::build(&config.llm, SCHEMA_API, PROMPT_API)?;
    let request = build_request(
        "Extract the public API from these C source files:",
        &RequestBody {
            files: request_files,
        },
    )?;

    let mut usage = LLMUsageTotals::default();
    let (response, u) = llm.invoke(&request)?;
    usage.add_usage(u.as_ref());
    info!("Token usage [api extraction] - {usage:?}");

    let api: ApiResponse = serde_json::from_str(&response)?;

    let sigs = api
        .functions
        .into_iter()
        .map(|f| {
            (
                f.name,
                FnSig {
                    return_type: f.return_type,
                    param_types: f.param_types,
                },
            )
        })
        .collect();

    let structs = api
        .structs
        .into_iter()
        .map(|s| {
            let key = s.name.trim_start_matches("struct").trim().to_string();
            let fields = s.fields.into_iter().map(|f| (f.name, f.ty)).collect();
            (key, (s.name, fields))
        })
        .collect();

    Ok((sigs, structs))
}

// ── C code generation ─────────────────────────────────────────────────────────

fn fill(template: &str, subs: &[(&str, &str)]) -> String {
    subs.iter().fold(template.to_string(), |s, (k, v)| {
        s.replace(&format!("{{{k}}}"), v)
    })
}

pub(crate) fn is_void_type(ty: &str) -> bool {
    ty.trim() == "void"
}

pub(crate) fn is_scalar_type(ty: &str) -> bool {
    matches!(
        ty.trim(),
        "int"
            | "unsigned int"
            | "long"
            | "unsigned long"
            | "long long"
            | "unsigned long long"
            | "short"
            | "unsigned short"
            | "char"
            | "unsigned char"
            | "signed char"
            | "float"
            | "double"
            | "long double"
            | "size_t"
            | "ssize_t"
            | "int8_t"
            | "int16_t"
            | "int32_t"
            | "int64_t"
            | "uint8_t"
            | "uint16_t"
            | "uint32_t"
            | "uint64_t"
            | "ptrdiff_t"
            | "intptr_t"
            | "uintptr_t"
            | "bool"
            | "_Bool"
    )
}

pub(crate) fn is_string_type(ty: &str) -> bool {
    matches!(
        ty.trim(),
        "char *" | "const char *" | "char*" | "const char*"
    )
}

fn generate_difftest_c(tests: &[TestVector], sigs: &HashMap<String, FnSig>) -> String {
    let mut out = TMPL_HEADER.to_string();

    for (i, test) in tests.iter().enumerate() {
        let test_id = format!("T{:03}", i + 1);
        let fn_name = &test.function;

        let Some(sig) = sigs.get(fn_name) else {
            out.push_str(&fill(
                TMPL_BODY_WARN,
                &[("TEST_ID", &test_id), ("FN_NAME", fn_name)],
            ));
            continue;
        };

        let ret = sig.return_type.trim();
        let param_type_str = if sig.param_types.is_empty() {
            "void".to_string()
        } else {
            sig.param_types.join(", ")
        };
        let arg_decls = sig
            .param_types
            .iter()
            .enumerate()
            .map(|(j, ty)| {
                let val = test.args.get(j).map(String::as_str).unwrap_or("0");
                format!("    {} arg{j} = {};\n", ty.trim(), val)
            })
            .collect::<String>();
        let args = (0..sig.param_types.len())
            .map(|j| format!("arg{j}"))
            .collect::<Vec<_>>()
            .join(", ");

        let subs = &[
            ("TEST_ID", test_id.as_str()),
            ("FN_NAME", fn_name.as_str()),
            ("RET", ret),
            ("PARAM_TYPES", param_type_str.as_str()),
            ("ARG_DECLS", arg_decls.as_str()),
            ("ARGS", args.as_str()),
        ];

        if is_void_type(ret) || is_scalar_type(ret) || is_string_type(ret) {
            let body = if is_void_type(ret) {
                TMPL_BODY_VOID
            } else if is_scalar_type(ret) {
                TMPL_BODY_SCALAR
            } else {
                TMPL_BODY_STRING
            };
            out.push_str(&fill(TMPL_PROLOGUE, subs));
            out.push_str(&fill(body, subs));
        } else {
            out.push_str(&fill(TMPL_BODY_WARN, subs));
        }
    }

    let test_calls = (0..tests.len())
        .map(|i| format!("    diff_T{:03}(&passed, &failed);\n", i + 1))
        .collect::<String>();

    out.push_str(&fill(TMPL_MAIN, &[("TEST_CALLS", &test_calls)]));
    out
}

// ── Tool implementation ───────────────────────────────────────────────────────

impl Tool for GenerateDiffTestSuite {
    fn name(&self) -> &'static str {
        "generate_difftest_suite"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(
            context
                .config
                .tools
                .get("generate_difftest_suite")
                .ok_or("generate_difftest_suite: missing config section")?,
        )?;

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("generate_difftest_suite: no RawSource in IR")?;

        let files = raw_source.dir.files_recursive();
        let (sigs, structs) = extract_c_api(&files, &config)?;
        info!(
            "Extracted {} public functions, {} struct types",
            sigs.len(),
            structs.len()
        );

        let tests = generate_test_vectors(&sigs, &structs);
        info!(
            "Generated {} test vectors across {} functions",
            tests.len(),
            tests
                .iter()
                .map(|t| t.function.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
        );

        let source = generate_difftest_c(&tests, &sigs);
        info!(
            "Generated difftest_suite.c ({} bytes):\n{}",
            source.len(),
            source
        );

        Ok(Box::new(DiffTestSuite { source }))
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub llm: LLMConfig,
    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.generate_difftest_suite", &self.unknown);
    }
}
