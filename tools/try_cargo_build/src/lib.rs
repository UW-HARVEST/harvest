//! Checks if a generated Rust project builds by materializing
//! it to a tempdir and running `cargo build --release`.
use full_source::CargoPackage;
use harvest_core::cargo_utils::{add_workspace_guard, normalize_package_name};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

pub struct TryCargoBuild;
// Either a vector of compiled artifact filenames (on success)
// or a string containing error messages (on failure).
pub type BuildResult = Result<Vec<PathBuf>, String>;

/// Parses cargo output stream and concatenates all compiler messages into a single string.
fn parse_compiler_messages(stdout: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let mut messages = Vec::new();

    for message in cargo_metadata::Message::parse_stream(stdout) {
        let message = message?;
        if let cargo_metadata::Message::CompilerMessage(comp_msg) = message {
            messages.push(format!("Compiler Message: {}", comp_msg));
        }
    }

    Ok(messages.join("\n"))
}

/// Parses cargo output stream and extracts the filenames of all compiled artifacts.
/// Returns a vector of PathBuf containing the artifact filenames.
fn parse_compiled_artifacts(stdout: &[u8]) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut artifact_filenames = Vec::new();

    for message in cargo_metadata::Message::parse_stream(stdout) {
        let message = message?;
        if let cargo_metadata::Message::CompilerArtifact(artifact) = message {
            // Extract filenames from all artifact files
            for filename in artifact.filenames {
                artifact_filenames.push(filename.into());
            }
        }
    }

    Ok(artifact_filenames)
}

/// Validates that the generated Rust project builds by running `cargo build --release`.
/// Note: It has a bit of a confusing return type:
/// - If the project builds successfully, it returns Ok(Ok(artifact_filenames)).
/// - If the project fails to build, it returns Ok(Err(error_message)).
/// - If there is an error running cargo, it returns Err.
fn try_cargo_build(project_path: &PathBuf) -> Result<BuildResult, Box<dyn std::error::Error>> {
    info!("Validating that the generated Rust project builds...");

    // Prevent accidentally picking up a parent workspace by marking this project as its own root.
    add_workspace_guard(&project_path.join("Cargo.toml"))?;
    // Normalize the package name to match the output directory so shared library names align with runner expectations.
    normalize_package_name(&project_path.join("Cargo.toml"), project_path)?;

    // Run cargo build in the project directory
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

    if output.status.success() {
        info!("Project builds successfully!");
        let artifact_filenames = parse_compiled_artifacts(&output.stdout)?;
        Ok(Ok(artifact_filenames))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let compiler_messages = parse_compiler_messages(&output.stdout)?;
        let error_message = format!("{}\n{}", compiler_messages, stderr);
        Ok(Err(error_message))
    }
}

impl Tool for TryCargoBuild {
    fn name(&self) -> &'static str {
        "try_cargo_build"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Get cargo package representation (the first and only arg of try_cargo_build)
        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[0])
            .ok_or("No CargoPackage representation found in IR")?;
        let output_path = context.config.output.clone();
        cargo_package.materialize(&output_path)?;

        // Validate that the Rust project builds
        let compilation_result = try_cargo_build(&output_path)?;
        let repr = CargoBuildResult {
            result: compilation_result,
        };
        Ok(Box::new(repr))
    }
}

/// A Representation that contains the results of running `cargo build`.
pub struct CargoBuildResult {
    pub result: Result<Vec<PathBuf>, String>,
}

impl std::fmt::Display for CargoBuildResult {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Built Rust artifact:")?;
        let artifact_filenames = match &self.result {
            Err(err) => return writeln!(f, "  Build failed: {err}"),
            Ok(filenames) => filenames,
        };
        writeln!(f, "  Build succeeded. Artifacts:")?;
        for filename in artifact_filenames {
            writeln!(f, "    {}", filename.display())?;
        }
        Ok(())
    }
}

impl Representation for CargoBuildResult {
    fn name(&self) -> &'static str {
        "cargo_build_result"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}
