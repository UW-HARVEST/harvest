//! Checks if the built Rust project passes the linter by
//! running `cargo clippy --fix`.
use full_source::CargoPackage;
use harvest_core::cargo_utils::{add_workspace_guard, normalize_package_name};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

pub struct TryClippyLint;
// Either a vector of clippy linter output (on success)
// or a string containing error messages (on failure).
pub type LintResult = Result<Vec<String>, String>;

/// Parses cargo clippy output stream and concatenates all linter messages into a single string.
fn parse_linter_messages(stdout: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let mut messages = Vec::new();

    for message in cargo_metadata::Message::parse_stream(stdout) {
        let message = message?;
        if let cargo_metadata::Message::CompilerMessage(comp_msg) = message {
            messages.push(format!("Linter Message: {}", comp_msg));
        }
    }

    Ok(messages.join("\n"))
}

/// Parses cargo clippy output stream and extracts linter output.
/// Returns a vector of PathBuf containing the artifact filenames.
fn parse_linted_artifacts(stdout: &[u8]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut linter_messages = Vec::new();
    println!("stdout {:?}\n", stdout);

    for message in cargo_metadata::Message::parse_stream(stdout) {
        let message = message?;
        println!("message here {:?}\n", message);
        if let cargo_metadata::Message::CompilerMessage(artifact) = message {
            // Extract linter messages from all messages
            println!("found message {}\n", artifact.message.message);
            linter_messages.push(artifact.message.message);
        } else {
            println!("not convertible\n");
        }
    }

    Ok(linter_messages)
}

fn clippy_error_output(output: &std::process::Output) -> Result<LintResult, Box<dyn std::error::Error>> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let compiler_messages = parse_linter_messages(&output.stdout)?;
    let error_message = format!("{}\n{}", compiler_messages, stderr);
    Ok(Err(error_message))
}

/// Checks and fixes the style of the output  by running `cargo clippy --fix`.
/// Note: It has a bit of a confusing return type:
/// - If the project passes the linter, it returns Ok(Ok(lint_output)).
/// - If the project fails the linter, it returns Ok(Err(error_message)).
/// - If there is an error running cargo, it returns Err.
fn try_clippy_lint(project_path: &PathBuf) -> Result<LintResult, Box<dyn std::error::Error>> {
    info!("Validating that the generated Rust project builds...");

    // Prevent accidentally picking up a parent workspace by marking this project as its own root.
    add_workspace_guard(&project_path.join("Cargo.toml"))?;
    // Normalize the package name to match the output directory so shared library names align with runner expectations.
    normalize_package_name(&project_path.join("Cargo.toml"), project_path)?;

    // Run clippy in the project directory once to get messages
    let output = Command::new("cargo")
        .arg("clippy")
        .arg("--message-format=json")
        .current_dir(project_path)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run cargo clippy in {}: {}",
                project_path.display(),
                e
            )
        })?;
    
    if output.status.success() {
        return clippy_error_output(&output);
    }

    // Run clippy a second time to auto-fix the linter issues
    let fix_output = Command::new("cargo")
        .arg("clippy")
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--message-format=json")
        .current_dir(project_path)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run cargo clippy --fix in {}: {}",
                project_path.display(),
                e
            )
        })?;

    if fix_output.status.success() {
        info!("Project linted successfully!");
        let lint_output = parse_linted_artifacts(&output.stdout)?;
        Ok(Ok(lint_output))
    } else {
        clippy_error_output(&fix_output)
    }
}

impl Tool for TryClippyLint {
    fn name(&self) -> &'static str {
        "try_clippy_lint"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Get cargo package representation (the first and only arg of try_clippy_lint)
        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("No CargoPackage representation found in IR")?;
        let output_path = context.config.output.clone();
        cargo_package.materialize(&output_path)?;

        // Validate that the Rust project passes the linter
        let linter_result = try_clippy_lint(&output_path)?;
        let repr = ClippyLintResult {
            result: linter_result,
        };
        Ok(Box::new(repr))
    }
}

/// A Representation that contains the results of running `cargo clippy` with JSON output.
pub struct ClippyLintResult {
    pub result: Result<Vec<String>, String>,
}

impl std::fmt::Display for ClippyLintResult {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Linter output artifact:")?;
        let artifact_messages = match &self.result {
            Err(err) => return writeln!(f, "  Linter failed: {err}"),
            Ok(message) => message,
        };
        writeln!(f, "  Linter passed. Linter output:")?;
        for message in artifact_messages {
            writeln!(f, "    {}", message)?;
        }
        Ok(())
    }
}

impl Representation for ClippyLintResult {
    fn name(&self) -> &'static str {
        "clippy_lint_result"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}
