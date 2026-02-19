//! Cargo compilation wrapper

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use tracing::{debug, trace};

/// Result of a cargo build
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub error_count: usize,
    pub warning_count: usize,
    pub combined_output: String,
}

/// Compile a Rust project using cargo
pub fn compile_project(project_dir: &Path) -> Result<BuildResult, Box<dyn std::error::Error>> {
    debug!("Compiling project at {}", project_dir.display());

    let output = Command::new("cargo")
        .arg("build")
        .arg("--color=never")
        .current_dir(project_dir)
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined_output = format!("{}\n{}", stdout, stderr);

    trace!("Build output:\n{}", combined_output);

    // Count errors and warnings
    let error_count = combined_output.matches("error:").count()
        + combined_output.matches("error[").count();
    let warning_count = combined_output.matches("warning:").count();

    let success = output.status.success();

    Ok(BuildResult {
        success,
        stdout,
        stderr,
        error_count,
        warning_count,
        combined_output,
    })
}
