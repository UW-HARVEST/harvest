use full_source::RawSource;
use generate_test_suite::TestSuite;
use harvest_core::config::unknown_field_warning;
use harvest_core::llm::{HarvestLLM, LLMConfig, LLMUsageTotals, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::info;

const STRUCTURED_OUTPUT_SCHEMA: &str = include_str!("structured_schema.json");
const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

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

        let test_suite = context
            .ir_snapshot
            .get::<TestSuite>(inputs[0])
            .ok_or("generate_difftest_suite: no TestSuite in IR")?;

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[1])
            .ok_or("generate_difftest_suite: no RawSource in IR")?;

        let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, SYSTEM_PROMPT)?;

        #[derive(Serialize)]
        struct InputFile {
            path: String,
            contents: String,
        }

        #[derive(Serialize)]
        struct RequestBody {
            c_source_files: Vec<InputFile>,
            test_suite: String,
        }

        let c_source_files = raw_source
            .dir
            .files_recursive()
            .into_iter()
            .map(|(path, contents)| InputFile {
                path: path.to_string_lossy().into_owned(),
                contents: String::from_utf8_lossy(contents).into_owned(),
            })
            .collect();

        let request = build_request(
            "Convert this test suite into a differential test harness using dlopen:",
            &RequestBody {
                c_source_files,
                test_suite: test_suite.source.clone(),
            },
        )?;

        let mut usage_totals = LLMUsageTotals::default();
        let (response, usage) = llm.invoke(&request)?;
        usage_totals.add_usage(usage.as_ref());

        #[derive(Deserialize)]
        struct Response {
            source: String,
        }

        let parsed: Response = serde_json::from_str(&response)?;

        info!(
            "Token usage [total] - prompt: {}, output: {}, total: {}",
            usage_totals.prompt_tokens, usage_totals.output_tokens, usage_totals.total_tokens
        );

        Ok(Box::new(DiffTestSuite { source: parsed.source }))
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
