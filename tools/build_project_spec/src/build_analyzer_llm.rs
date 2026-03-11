//! LLM abstraction layer for build target analysis.
//! Abstracts prompt/request construction and provides a typed response interface.

use crate::{Config, ProjectKind};
use harvest_core::llm::{HarvestLLM, LLMUsageTotals, Usage, build_request};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

const STRUCTURED_OUTPUT_SCHEMA: &str = include_str!("prompts/structured_schema.json");
const SYSTEM_PROMPT: &str = include_str!("prompts/system_prompt.txt");

/// Typed target spec returned by the build analyzer LLM.
#[derive(Debug, Deserialize)]
pub struct BuildAnalysisTarget {
    /// Logical CMake target name (e.g. from `add_library`/`add_executable`).
    pub name: String,
    /// Produced artifact path relative to the project root.
    pub artifact: String,
    /// Target kind inferred from the build system. (Library vs Executable)
    pub kind: ProjectKind,
    /// Source/header files directly compiled for this target (project-root-relative).
    pub sources: Vec<String>,
    /// Direct logical target dependencies by CMake target name.
    pub deps: Vec<String>,
}

/// Typed full response returned by the build analyzer LLM.
#[derive(Debug, Deserialize)]
pub struct BuildAnalysisResponse {
    pub targets: Vec<BuildAnalysisTarget>,
}

#[derive(Serialize)]
struct BuildAnalysisRequest<'a> {
    file_includes: &'a HashMap<String, Vec<String>>,
    cmakelists: &'a HashMap<String, String>,
    compile_commands_json: Option<&'a str>,
}

pub struct BuildAnalyzerLLM {
    llm: HarvestLLM,
    usage_totals: Mutex<LLMUsageTotals>,
}

impl BuildAnalyzerLLM {
    pub fn build(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let llm = HarvestLLM::build(&config.llm, STRUCTURED_OUTPUT_SCHEMA, SYSTEM_PROMPT)?;
        Ok(Self {
            llm,
            usage_totals: Mutex::new(LLMUsageTotals::default()),
        })
    }

    fn record_usage(&self, usage: Option<&Usage>) {
        let mut usage_totals = self
            .usage_totals
            .lock()
            .expect("usage mutex poisoned in build analyzer");
        usage_totals.add_usage(usage);
    }

    pub fn usage_totals(&self) -> LLMUsageTotals {
        *self
            .usage_totals
            .lock()
            .expect("usage mutex poisoned in build analyzer")
    }

    pub fn analyze_project(
        &self,
        file_includes: &HashMap<String, Vec<String>>,
        cmakelists: &HashMap<String, String>,
        compile_commands_json: Option<&str>,
    ) -> Result<BuildAnalysisResponse, Box<dyn std::error::Error>> {
        let request = build_request(
            "Infer produced artifacts and project kinds from the source tree and CMakeLists content.",
            &BuildAnalysisRequest {
                file_includes,
                cmakelists,
                compile_commands_json,
            },
        )?;

        let (response, usage) = self.llm.invoke(&request)?;
        self.record_usage(usage.as_ref());

        Ok(serde_json::from_str(&response)?)
    }
}
