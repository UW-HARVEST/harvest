//! LLM-based file fixing

use crate::error_classifier::FileErrorReport;
use harvest_core::llm::{HarvestLLM, LLMConfig, ChatMessage};
use serde::Serialize;
use std::fs;
use std::path::Path;
use tracing::{debug, trace};

const FIXER_PROMPT: &str = include_str!("../system_prompts/fixer.txt");

/// Fix a single file using LLM
pub fn fix_file(
    project_root: &Path,
    file_report: &FileErrorReport,
    llm_config: &LLMConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let file_path = project_root.join(&file_report.file_path);

    debug!("Fixing file: {}", file_path.display());

    // Read current content
    let current_content = fs::read_to_string(&file_path)
        .map_err(|e| format!("Failed to read {}: {}", file_path.display(), e))?;

    // Build LLM client (no JSON schema - plain text output)
    let llm = HarvestLLM::build(llm_config, None, FIXER_PROMPT)?;

    // Prepare request
    #[derive(Serialize)]
    struct ErrorInfo {
        error_type: String,
        line: Option<usize>,
        message: String,
        code: Option<String>,
    }

    #[derive(Serialize)]
    struct RequestBody {
        file_path: String,
        current_content: String,
        errors: Vec<ErrorInfo>,
    }

    let errors: Vec<ErrorInfo> = file_report.errors.iter()
        .map(|e| ErrorInfo {
            error_type: e.error_type.clone(),
            line: e.line,
            message: e.message.clone(),
            code: e.code.clone(),
        })
        .collect();

    let request_body = RequestBody {
        file_path: file_report.file_path.clone(),
        current_content,
        errors,
    };

    let request_json = serde_json::to_string(&request_body)?;

    // Build request messages
    let request = vec![
        ChatMessage::user()
            .content("Fix the following Rust file to resolve compilation errors:")
            .build(),
        ChatMessage::user()
            .content(&request_json)
            .build(),
    ];

    // Make LLM call
    trace!("Calling LLM to fix {}", file_report.file_path);
    let fixed_content = llm.invoke(&request)?;
    trace!("LLM returned {} bytes", fixed_content.len());

    // Strip any accidental markdown fences
    let fixed_content = fixed_content.trim();
    let fixed_content = fixed_content.strip_prefix("```").unwrap_or(fixed_content);
    let fixed_content = fixed_content.strip_prefix("rust").unwrap_or(fixed_content);
    let fixed_content = fixed_content.strip_suffix("```").unwrap_or(fixed_content);
    let fixed_content = fixed_content.trim();

    // Write fixed content
    fs::write(&file_path, fixed_content)
        .map_err(|e| format!("Failed to write {}: {}", file_path.display(), e))?;

    debug!("Successfully wrote fixed content to {}", file_path.display());

    Ok(())
}
