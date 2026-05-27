use full_source::RawSource;
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::{HarvestLLM, LLMConfig, LLMUsageTotals, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

const SCHEMA: &str = include_str!("structured_schema_inputs.json");
const PROMPT: &str = include_str!("system_prompt_inputs.txt");

// ── Public types ──────────────────────────────────────────────────────────────

/// One set of inputs for a single executable invocation.
#[derive(Serialize, Deserialize)]
pub struct TestInput {
    pub argv: Vec<String>,
    pub stdin: String,
}

/// LLM-generated test inputs for an executable project.
pub struct ExecTestInputs {
    pub cases: Vec<TestInput>,
}

impl std::fmt::Display for ExecTestInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "ExecTestInputs({} cases)", self.cases.len())
    }
}

impl Representation for ExecTestInputs {
    fn name(&self) -> &'static str {
        "exec_test_inputs"
    }
}

pub struct GenerateExecDifftests;

// ── LLM request/response types ────────────────────────────────────────────────

#[derive(Serialize)]
struct InputFile {
    path: String,
    contents: String,
}

#[derive(Serialize)]
struct RequestBody {
    files: Vec<InputFile>,
}

#[derive(Deserialize)]
struct LlmResponse {
    test_cases: Vec<TestInput>,
}

// ── Tool implementation ───────────────────────────────────────────────────────

impl Tool for GenerateExecDifftests {
    fn name(&self) -> &'static str {
        "generate_exec_difftests"
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
                .get("generate_exec_difftests")
                .ok_or("generate_exec_difftests: missing config section")?,
        )?;

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("generate_exec_difftests: no RawSource in IR")?;

        let files = raw_source.dir.files_recursive();
        let request_files = files
            .iter()
            .map(|(path, contents)| InputFile {
                path: path.to_string_lossy().into_owned(),
                contents: String::from_utf8_lossy(contents).into_owned(),
            })
            .collect();

        let llm = HarvestLLM::build(&config.llm, SCHEMA, PROMPT)?;
        let request = build_request(
            "Generate test inputs for this C program:",
            &RequestBody {
                files: request_files,
            },
        )?;

        let mut usage = LLMUsageTotals::default();
        let (response, u) = llm.invoke(&request)?;
        usage.add_usage(u.as_ref());
        info!("Token usage [exec test input generation] - {usage}");

        let parsed: LlmResponse = serde_json::from_str(&response)?;
        info!(
            "generate_exec_difftests: generated {} test inputs",
            parsed.test_cases.len()
        );

        Ok(Box::new(ExecTestInputs {
            cases: parsed.test_cases,
        }))
    }
}

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub llm: LLMConfig,
    #[serde(flatten)]
    unknown: HashMap<String, serde_json::Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.generate_exec_difftests", &self.unknown);
    }
}
