//! Checks if a generated Rust project builds by materializing
//! it to a tempdir and running `cargo build --release`.
pub use cargo_metadata::{Artifact, CompilerMessage};
use full_source::CargoPackage;
use harvest_core::cargo_utils::CargoToml;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

pub struct TryCargoBuild;
// Either a vector of compiled artifact filenames (on success)
// or a string containing error messages (on failure).
pub type BuildResult = Result<Vec<PathBuf>, Vec<CompilerMessage>>;

/// Validates that the generated Rust project builds by running `cargo build --release`.
/// When `features` is non-empty, build with `--no-default-features --features <list>` so
/// the produced binary matches the variant the test case expects.
/// Note: It has a bit of a confusing return type:
/// - If the project builds successfully, it returns Ok(Ok(artifact_filenames)).
/// - If the project fails to build, it returns Ok(Err(error_message)).
/// - If there is an error running cargo, it returns Err.
fn try_cargo_build(
    project_path: &PathBuf,
    features: &[String],
) -> Result<CargoBuildResult, Box<dyn std::error::Error>> {
    info!("Validating that the generated Rust project builds...");

    let mut cargo = CargoToml::open(&project_path.join("Cargo.toml"))?;
    cargo.add_workspace();
    cargo.normalize_name(project_path);
    cargo.save()?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("--message-format=json");
    if !features.is_empty() {
        info!("Using test-derived features: {}", features.join(","));
        cmd.arg("--no-default-features")
            .arg("--features")
            .arg(features.join(","));
    }
    let output = cmd
        .current_dir(project_path)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run cargo build in {}: {}",
                project_path.display(),
                e
            )
        })?;

    let mut artifacts = vec![];
    let mut diagnostics = vec![];
    let mut success = false;
    for message in cargo_metadata::Message::parse_stream(output.stdout.as_slice()) {
        let message = message?;
        match message {
            // Compiled artifacts for a particular target
            cargo_metadata::Message::CompilerArtifact(artifact) => artifacts.push(artifact),
            cargo_metadata::Message::CompilerMessage(compiler_message) => {
                diagnostics.push(compiler_message)
            }
            cargo_metadata::Message::BuildFinished(build_finished) => {
                success = build_finished.success
            }
            // Ignore the following variants for now
            cargo_metadata::Message::BuildScriptExecuted(_) => {}
            cargo_metadata::Message::TextLine(_) => {}
            // Non-exhaustive pattern, so need a catch-all
            _ => {}
        }
    }

    if success {
        info!("Project builds successfully!");
    }
    Ok(CargoBuildResult {
        artifacts,
        diagnostics,
        success,
        err: String::from_utf8(output.stderr)?,
    })
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

        let features = read_test_features(&context.config.input);

        // Validate that the Rust project builds
        Ok(Box::new(try_cargo_build(&output_path, &features)?))
    }
}

/// Read Cargo features from `<input>/CMakePresets.json`.
///
/// Test cases declare the required build configuration via CMake presets:
///   "cacheVariables": { "HASH_BACKEND": "blake", "SECPAR": "128f", "THASH": "simple" }
///
/// We assume the agent translates the C `#ifdef` switches into Cargo features whose
/// names match the cache-variable values exactly (e.g. `blake`, `128f`, `simple`).
/// CMake-builtin keys (those starting with `CMAKE_`) are filtered out.
///
/// Returns an empty Vec when no presets file exists, no relevant keys are found, or
/// parsing fails — in which case the build falls back to default features.
fn read_test_features(input_dir: &Path) -> Vec<String> {
    let presets_path = input_dir.join("CMakePresets.json");
    let Ok(content) = std::fs::read_to_string(&presets_path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        tracing::warn!("Failed to parse {}; using default features", presets_path.display());
        return Vec::new();
    };

    // Collect all configurePresets in a name->preset map for inheritance lookup.
    let presets = json
        .get("configurePresets")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let by_name: std::collections::HashMap<String, &serde_json::Value> = presets
        .iter()
        .filter_map(|p| Some((p.get("name")?.as_str()?.to_string(), p)))
        .collect();

    // Prefer a preset literally named "test"; otherwise pick the last non-hidden one.
    let target = by_name.get("test").copied().or_else(|| {
        presets
            .iter()
            .filter(|p| !p.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false))
            .last()
    });
    let Some(target) = target else { return Vec::new() };

    // Walk the inherits chain, merging cacheVariables (later overrides earlier).
    let mut merged: std::collections::BTreeMap<String, String> = Default::default();
    fn walk(
        node: &serde_json::Value,
        by_name: &std::collections::HashMap<String, &serde_json::Value>,
        merged: &mut std::collections::BTreeMap<String, String>,
    ) {
        if let Some(inherits) = node.get("inherits") {
            let parents: Vec<&str> = match inherits {
                serde_json::Value::String(s) => vec![s.as_str()],
                serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_str()).collect(),
                _ => Vec::new(),
            };
            for p in parents {
                if let Some(parent) = by_name.get(p) {
                    walk(parent, by_name, merged);
                }
            }
        }
        if let Some(vars) = node.get("cacheVariables").and_then(|v| v.as_object()) {
            for (k, v) in vars {
                if let Some(s) = v.as_str() {
                    merged.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    walk(target, &by_name, &mut merged);

    merged
        .into_iter()
        .filter(|(k, _)| !k.starts_with("CMAKE_"))
        .map(|(_, v)| v)
        .collect()
}

/// A Representation that contains the results of running `cargo build`.
#[derive(Clone)]
pub struct CargoBuildResult {
    pub artifacts: Vec<Artifact>,
    pub diagnostics: Vec<CompilerMessage>,
    pub success: bool,
    pub err: String,
}

impl std::fmt::Display for CargoBuildResult {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Built Rust artifact:")?;
        if self.success {
            writeln!(f, "  Build succeeded. Artifacts:")?;
            for filename in self.artifacts.iter().flat_map(|a| &a.filenames) {
                writeln!(f, "    {}", filename)?;
            }
            Ok(())
        } else {
            writeln!(f, "  Build failed: {}", self.err)
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_presets(dir: &Path, json: &str) {
        fs::write(dir.join("CMakePresets.json"), json).unwrap();
    }

    #[test]
    fn test_features_from_test_preset_with_inheritance() {
        let tmp = tempfile::tempdir().unwrap();
        write_presets(
            tmp.path(),
            r#"{
              "configurePresets": [
                {
                  "name": "base", "hidden": true,
                  "cacheVariables": { "CMAKE_BUILD_TYPE": "Release" }
                },
                {
                  "name": "test", "inherits": "base",
                  "cacheVariables": {
                    "HASH_BACKEND": "blake",
                    "SECPAR": "128f",
                    "THASH": "simple"
                  }
                }
              ]
            }"#,
        );
        let mut features = read_test_features(tmp.path());
        features.sort();
        assert_eq!(features, vec!["128f", "blake", "simple"]);
    }

    #[test]
    fn test_features_missing_presets_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_test_features(tmp.path()).is_empty());
    }

    #[test]
    fn test_features_only_cmake_builtins_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write_presets(
            tmp.path(),
            r#"{
              "configurePresets": [
                {
                  "name": "test",
                  "cacheVariables": {
                    "CMAKE_C_STANDARD": "99",
                    "CMAKE_BUILD_TYPE": "Release"
                  }
                }
              ]
            }"#,
        );
        assert!(read_test_features(tmp.path()).is_empty());
    }
}
