//! LLM-based file fixing

use harvest_core::llm::{ChatMessage, HarvestLLM, LLMConfig};
use serde::Serialize;
use std::fs;
use std::path::Path;
use tracing::{debug, trace};

const FIXER_PROMPT: &str = include_str!("../system_prompts/fixer.txt");

/// Fix a single file using LLM, given only the diagnostics for that file.
pub fn fix_file(
    project_root: &Path,
    file_path: &str,
    file_errors: &str,
    llm_config: &LLMConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let abs_path = project_root.join(file_path);

    debug!("Fixing file: {}", abs_path.display());

    let current_content = fs::read_to_string(&abs_path)
        .map_err(|e| format!("Failed to read {}: {}", abs_path.display(), e))?;

    let llm = HarvestLLM::build(llm_config, None, FIXER_PROMPT)?;

    #[derive(Serialize)]
    struct FileBody<'a> {
        target_file: &'a str,
        current_content: &'a str,
    }

    let file_body = FileBody {
        target_file: file_path,
        current_content: &current_content,
    };

    let request = vec![
        ChatMessage::user()
            .content("Fix the compilation errors in the target Rust file specified below.")
            .build(),
        ChatMessage::user().content(file_errors).build(),
        ChatMessage::user()
            .content(&serde_json::to_string(&file_body)?)
            .build(),
    ];

    trace!("Calling LLM to fix {}", file_path);
    let fixed_content = llm.invoke(&request)?;
    trace!("LLM returned {} bytes", fixed_content.len());

    let fixed_content = fixed_content.trim();

    fs::write(&abs_path, fixed_content)
        .map_err(|e| format!("Failed to write {}: {}", abs_path.display(), e))?;

    debug!("Successfully wrote fixed content to {}", abs_path.display());

    Ok(())
}
