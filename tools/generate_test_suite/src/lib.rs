use full_source::RawSource;
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

/// A generated C test suite that exercises the public API of the C library.
pub struct TestSuite {
    pub source: String,
}

impl std::fmt::Display for TestSuite {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.source)
    }
}

impl Representation for TestSuite {
    fn name(&self) -> &'static str {
        "test_suite"
    }
}

pub struct GenerateTestSuite;

impl Tool for GenerateTestSuite {
    fn name(&self) -> &'static str {
        "generate_test_suite"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(context.config.tools.get("generate_test_suite").unwrap())?;

        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("generate_test_suite: no RawSource in IR")?;

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

        let files = raw_source
            .dir
            .files_recursive()
            .into_iter()
            .map(|(path, contents)| InputFile {
                path: path.to_string_lossy().into_owned(),
                contents: String::from_utf8_lossy(contents).into_owned(),
            })
            .collect();

        let request = build_request(
            "Generate a comprehensive test suite for this C library:",
            &RequestBody { files },
        )?;

        let mut usage_totals = LLMUsageTotals::default();
        let (response, usage) = llm.invoke(&request)?;
        usage_totals.add_usage(usage.as_ref());

        #[derive(Deserialize)]
        struct Response {
            source: String,
        }

        let parsed: Response = serde_json::from_str(&response)?;

        info!("Token usage [total] - {usage_totals}");
        info!(
            "Generated test_suite.c ({} bytes):\n{}",
            parsed.source.len(),
            parsed.source
        );

        Ok(Box::new(TestSuite {
            source: parsed.source,
        }))
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
        unknown_field_warning("tools.generate_test_suite", &self.unknown);
    }
}
