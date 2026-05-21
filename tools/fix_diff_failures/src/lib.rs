use full_source::{CargoPackage, RawSource};
use harvest_core::config::unknown_field_warning;
use harvest_core::fs::RawDir;
use harvest_core::llm::{HarvestLLM, LLMConfig, LLMUsageTotals, build_request};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use run_difftest::DiffTestResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

const STRUCTURED_OUTPUT_SCHEMA: &str = include_str!("structured_schema.json");
const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

pub struct FixDiffFailures;

impl Tool for FixDiffFailures {
    fn name(&self) -> &'static str {
        "fix_diff_failures"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(context.config.tools.get("fix_diff_failures").unwrap())?;

        let diff_result = context
            .ir_snapshot
            .get::<DiffTestResult>(inputs[0])
            .ok_or("fix_diff_failures: no DiffTestResult in IR")?;
        let raw_source = context
            .ir_snapshot
            .get::<RawSource>(inputs[1])
            .ok_or("fix_diff_failures: no RawSource in IR")?;
        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[2])
            .ok_or("fix_diff_failures: no CargoPackage in IR")?;

        let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, SYSTEM_PROMPT)?;

        #[derive(Serialize)]
        struct SourceFile {
            path: String,
            contents: String,
        }

        #[derive(Serialize)]
        struct RequestBody {
            c_source_files: Vec<SourceFile>,
            rust_source_files: Vec<SourceFile>,
            failures: Vec<String>,
        }

        let c_source_files = raw_source
            .dir
            .files_recursive()
            .into_iter()
            .map(|(path, contents)| SourceFile {
                path: path.to_string_lossy().into_owned(),
                contents: String::from_utf8_lossy(contents).into_owned(),
            })
            .collect();

        let rust_source_files = cargo_package
            .dir
            .files_recursive()
            .into_iter()
            .map(|(path, contents)| SourceFile {
                path: path.to_string_lossy().into_owned(),
                contents: String::from_utf8_lossy(contents).into_owned(),
            })
            .collect();

        let request = build_request(
            "Fix the Rust code to match C behavior for the following failures:",
            &RequestBody {
                c_source_files,
                rust_source_files,
                failures: diff_result.failures.clone(),
            },
        )?;

        let mut usage_totals = LLMUsageTotals::default();
        let (response, usage) = llm.invoke(&request)?;
        usage_totals.add_usage(usage.as_ref());

        #[derive(Deserialize)]
        struct OutputFiles {
            files: Vec<OutputFile>,
        }

        #[derive(Deserialize)]
        struct OutputFile {
            path: PathBuf,
            contents: String,
        }

        let files: OutputFiles = serde_json::from_str(&response)?;
        info!("LLM returned {} files", files.files.len());

        let mut out_dir = RawDir::default();
        for file in files.files {
            out_dir.set_file(&file.path, file.contents.into())?;
        }

        info!("Token usage [total] - {usage_totals}");

        Ok(Box::new(CargoPackage { dir: out_dir }))
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
        unknown_field_warning("tools.fix_diff_failures", &self.unknown);
    }
}
