//! LLM-based error classification

use crate::compiler::BuildResult;
use harvest_core::llm::{build_request, HarvestLLM, LLMConfig};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{debug, trace};

const CLASSIFIER_PROMPT: &str = include_str!("../system_prompts/classifier.txt");

/// Classification of errors by file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorClassification {
    pub files: Vec<FileErrorReport>,
    pub summary: String,
}

/// Errors for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileErrorReport {
    pub file_path: String,
    pub priority: u32,
    pub errors: Vec<FileError>,
}

/// A single error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileError {
    #[serde(default)]
    pub error_type: String,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub code: Option<String>,
}

/// Classify compilation errors using LLM
pub fn classify_errors(
    build_result: &BuildResult,
    llm_config: &LLMConfig,
    debug_dir: Option<&Path>,
) -> Result<ErrorClassification, Box<dyn std::error::Error>> {
    debug!("Classifying {} errors", build_result.error_count);

    // Build LLM client with JSON schema
    let schema = r#"{
        "name": "error_classification",
        "description": "Classification of compilation errors by file",
        "strict": true,
        "schema": {
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file_path": {"type": "string"},
                            "priority": {"type": "number"},
                            "errors": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "error_type": {"type": "string"},
                                        "line": {"type": "number"},
                                        "message": {"type": "string"},
                                        "code": {"type": "string"}
                                    },
                                    "required": ["message"]
                                }
                            }
                        },
                        "required": ["file_path", "priority", "errors"]
                    }
                },
                "summary": {"type": "string"}
            },
            "required": ["files", "summary"]
        }
    }"#;

    let llm = HarvestLLM::build(llm_config, Some(schema), CLASSIFIER_PROMPT)?;

    // Prepare request
    #[derive(Serialize)]
    struct RequestBody {
        build_output: String,
    }

    let request = build_request(
        "Analyze this compilation output and classify errors by file:",
        &RequestBody {
            build_output: build_result.combined_output.clone(),
        },
    )?;

    // Make LLM call
    trace!("Calling LLM for error classification");
    let response = llm.invoke(&request)?;
    debug!("LLM response received: {} bytes", response.len());

    // Save response for debugging if debug_dir is provided
    if let Some(dir) = debug_dir {
        let response_path = dir.join("classification_response.json");
        if let Err(e) = std::fs::write(&response_path, &response) {
            debug!("Failed to save classification response: {}", e);
        } else {
            debug!("Saved classification response to {}", response_path.display());
        }
    }

    // Parse response
    let classification: ErrorClassification = serde_json::from_str(&response)
        .map_err(|e| format!("Failed to parse classification JSON: {}\nResponse: {}", e, response))?;

    // Sort files by priority
    let mut sorted_classification = classification;
    sorted_classification.files.sort_by_key(|f| f.priority);

    debug!("Classified errors into {} files", sorted_classification.files.len());
    for file in &sorted_classification.files {
        debug!("  {} (priority {}, {} errors)",
               file.file_path, file.priority, file.errors.len());
    }

    Ok(sorted_classification)
}
