use full_source::RawSource;
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::{HarvestLLM, LLMConfig, LLMUsageTotals, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

const STRUCTURED_OUTPUT_SCHEMA: &str = include_str!("structured_schema.json");
const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

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

#[derive(Debug, Deserialize)]
struct TestVector {
    function: String,
    args: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TestVectors {
    tests: Vec<TestVector>,
}

/// Parsed C function signature.
struct FnSig {
    return_type: String,
    param_types: Vec<String>,
}

/// Given a single parameter declaration (e.g. `const char *s`, `int a`, `size_t`),
/// strip the trailing name (if any) and return just the type.
fn strip_param_name(param: &str) -> String {
    let param = param.trim();
    if param.is_empty() || param == "void" {
        return param.to_string();
    }
    // Find the start of the last identifier — that is the parameter name.
    // Everything before it (trimmed) is the type.
    let name_start = param
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    if name_start == 0 {
        // Single token — it's an anonymous parameter, the whole thing is the type.
        param.to_string()
    } else {
        param[..name_start].trim_end().to_string()
    }
}

/// Split `return_type fn_name` (the text before the `(` of a declaration) into its two parts.
fn split_return_and_name(left: &str) -> Option<(String, String)> {
    let left = left.trim();
    let name_start = left
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);
    let fn_name = left[name_start..].to_string();
    let ret_type = left[..name_start].trim().to_string();
    if fn_name.is_empty() || ret_type.is_empty() {
        return None;
    }
    Some((ret_type, fn_name))
}

/// Parse public function signatures from the raw C source files.
/// Each line of the form `ret_type fn_name(params) {` or `...;` that does not start
/// with `static` is treated as a function declaration.
fn parse_c_signatures(files: &[(PathBuf, &[u8])]) -> HashMap<String, FnSig> {
    const SKIP_NAMES: &[&str] = &[
        "if", "for", "while", "switch", "do", "return", "sizeof", "else", "typedef", "struct",
        "enum", "union",
    ];

    let mut sigs = HashMap::new();

    for (_, contents) in files {
        let src = String::from_utf8_lossy(contents);
        for line in src.lines() {
            let trimmed = line.trim();

            // Skip preprocessor directives, comments, and static/typedef definitions.
            if trimmed.starts_with('#')
                || trimmed.starts_with("//")
                || trimmed.starts_with("/*")
                || trimmed.starts_with('*')
                || trimmed.starts_with("static")
            {
                continue;
            }

            // Must have `(` followed somewhere by `)`.
            let Some(paren_open) = trimmed.find('(') else {
                continue;
            };
            let Some(rel_close) = trimmed[paren_open..].find(')') else {
                continue;
            };
            let paren_close = paren_open + rel_close;

            // After `)` must come `{` or `;` (function definition or declaration).
            let after = trimmed[paren_close + 1..].trim();
            if !after.starts_with('{') && !after.starts_with(';') {
                continue;
            }

            let left = trimmed[..paren_open].trim();
            let params_str = trimmed[paren_open + 1..paren_close].trim();

            let Some((ret_type, fn_name)) = split_return_and_name(left) else {
                continue;
            };

            if SKIP_NAMES.contains(&fn_name.as_str()) {
                continue;
            }

            let param_types: Vec<String> = if params_str.is_empty() || params_str == "void" {
                vec![]
            } else {
                params_str.split(',').map(strip_param_name).collect()
            };

            sigs.insert(fn_name, FnSig { return_type: ret_type, param_types });
        }
    }

    sigs
}

fn fill(template: &str, subs: &[(&str, &str)]) -> String {
    subs.iter().fold(template.to_string(), |s, (k, v)| {
        s.replace(&format!("{{{k}}}"), v)
    })
}

fn is_void_type(ty: &str) -> bool {
    ty.trim() == "void"
}

fn is_scalar_type(ty: &str) -> bool {
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

fn is_string_type(ty: &str) -> bool {
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

impl Tool for GenerateDiffTestSuite {
    fn name(&self) -> &'static str {
        "generate_difftest_suite"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config =
            Config::deserialize(context.config.tools.get("generate_difftest_suite").unwrap())?;

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("generate_difftest_suite: no RawSource in IR")?;

        let files = raw_source.dir.files_recursive();
        let sigs = parse_c_signatures(&files);
        info!("Parsed signatures for {} public functions", sigs.len());

        let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, SYSTEM_PROMPT)?;

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

        let request = build_request(
            "Generate test vectors for every public function in this C library:",
            &RequestBody { files: request_files },
        )?;

        let mut usage_totals = LLMUsageTotals::default();
        let (response, usage) = llm.invoke(&request)?;
        usage_totals.add_usage(usage.as_ref());

        let vectors: TestVectors = serde_json::from_str(&response)?;

        info!("Token usage [total] - {usage_totals}");
        info!(
            "Generated {} test vectors for {} functions",
            vectors.tests.len(),
            vectors
                .tests
                .iter()
                .map(|t| t.function.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
        );

        let source = generate_difftest_c(&vectors.tests, &sigs);

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
