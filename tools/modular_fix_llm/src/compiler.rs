//! Runs `cargo build` in a given directory and captures the raw output.

use harvest_core::cargo_utils::{add_workspace_guard, normalize_package_name};
use std::path::Path;
use std::process::Command;
use tracing::debug;

/// Raw output from a `cargo build --message-format=json` invocation.
pub struct BuildOutput {
    pub success: bool,
    /// Raw JSON-lines stdout for parsing with `cargo_metadata`.
    pub stdout_bytes: Vec<u8>,
}

/// Run `cargo build --release --message-format=json` in `project_path`.
///
/// Also applies `add_workspace_guard` and `normalize_package_name` so the
/// build can succeed in an isolated temp directory.
pub fn run_cargo_build(project_path: &Path) -> Result<BuildOutput, Box<dyn std::error::Error>> {
    add_workspace_guard(&project_path.join("Cargo.toml"))?;
    normalize_package_name(&project_path.join("Cargo.toml"), project_path)?;

    debug!("Running cargo build in {}", project_path.display());

    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--message-format=json")
        .current_dir(project_path)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run cargo build in {}: {}",
                project_path.display(),
                e
            )
        })?;

    Ok(BuildOutput {
        success: output.status.success(),
        stdout_bytes: output.stdout,
    })
}
