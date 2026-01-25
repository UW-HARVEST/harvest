//! Checks if a generated Rust project builds by materializing
//! it to a tempdir and running `cargo build --release`.
use crate::tools::raw_source_to_cargo_llm::CargoPackage;
use crate::tools::{MightWriteContext, MightWriteOutcome, RunContext, Tool};
use harvest_ir::{HarvestIR, Representation, fs::RawDir};
use std::fs;
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
    add_local_workspace_guard(&project_path.join("Cargo.toml"))?;
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
/// Returns the CargoPackage representation in IR.
/// If there is not exactly 1 CargoPackage representation,
/// return an error.
fn raw_cargo_package(ir: &HarvestIR) -> Result<&RawDir, Box<dyn std::error::Error>> {
    let cargo_packages: Vec<&RawDir> = ir
        .get_by_representation::<CargoPackage>()
        .map(|(_, r)| &r.dir)
        .collect();

    match cargo_packages.len() {
        0 => Err("No CargoPackage representation found in IR".into()),
        1 => Ok(cargo_packages[0]),
        n => Err(format!(
            "Found {} CargoPackage representations, expected at most 1",
            n
        )
        .into()),
    }
}

impl Tool for TryCargoBuild {
    fn name(&self) -> &'static str {
        "try_cargo_build"
    }

    fn might_write(&mut self, context: MightWriteContext) -> MightWriteOutcome {
        // We need a cargo_package to be available, but we won't write any existing IDs.
        match raw_cargo_package(context.ir) {
            Err(_) => MightWriteOutcome::TryAgain,
            Ok(_) => MightWriteOutcome::Runnable([].into()),
        }
    }

    fn run(self: Box<Self>, context: RunContext) -> Result<(), Box<dyn std::error::Error>> {
        // Get cargo package representation
        let cargo_package = raw_cargo_package(&context.ir_snapshot)?;
        let output_path = context.config.output.clone();
        cargo_package.materialize(&output_path)?;

        // Validate that the Rust project builds
        let compilation_result = try_cargo_build(&output_path)?;
        // Write result to IR
        context
            .ir_edit
            .add_representation(Box::new(CargoBuildResult {
                result: compilation_result,
            }));

        Ok(())
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
        "CargoBuildResult"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

/// Add an empty `[workspace]` section to the manifest if one is not present, so that
/// Cargo treats the generated project as a workspace root instead of trying to join
/// any parent workspace (which would fail because this package is not listed).
fn add_local_workspace_guard(manifest: &Path) -> std::io::Result<()> {
    if !manifest.exists() {
        return Ok(());
    }
    let contents = fs::read_to_string(manifest)?;
    if contents.contains("\n[workspace]") || contents.trim_start().starts_with("[workspace]") {
        return Ok(());
    }
    let mut updated = contents;
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("[workspace]\n");
    fs::write(manifest, updated)
}

/// Force the package name to match the output directory name. This keeps the produced
/// `lib<name>.so` aligned with the test runner's expected library stem.
fn normalize_package_name(manifest: &Path, project_dir: &Path) -> std::io::Result<()> {
    if !manifest.exists() {
        return Ok(());
    }
    let desired = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if desired.is_empty() {
        return Ok(());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut lines = Vec::new();
    let mut in_package = false;
    let mut changed = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            lines.push(line.to_string());
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            lines.push(format!("name = \"{}\"", desired));
            changed = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if changed {
        let mut updated = lines.join("\n");
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        fs::write(manifest, updated)?;
    }
    Ok(())
}
