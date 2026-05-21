use build_c_library::CLibraryArtifact;
use full_source::CargoPackage;
use generate_difftest_suite::DiffTestSuite;
use harvest_core::cargo_utils::CargoToml;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

/// The results of a differential test run comparing C and Rust library outputs.
pub struct DiffTestResult {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    /// Raw FAIL and WARN lines from the test run, for use by the fix tool.
    pub failures: Vec<String>,
}

impl std::fmt::Display for DiffTestResult {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "DiffTestResult: {}/{} passed", self.passed, self.total)?;
        for failure in &self.failures {
            writeln!(f, "  {failure}")?;
        }
        Ok(())
    }
}

impl Representation for DiffTestResult {
    fn name(&self) -> &'static str {
        "diff_test_result"
    }
}

pub struct RunDiffTest;

impl Tool for RunDiffTest {
    fn name(&self) -> &'static str {
        "run_difftest"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let diff_test_suite = context
            .ir_snapshot
            .get::<DiffTestSuite>(inputs[0])
            .ok_or("run_difftest: no DiffTestSuite in IR")?;
        let c_library = context
            .ir_snapshot
            .get::<CLibraryArtifact>(inputs[1])
            .ok_or("run_difftest: no CLibraryArtifact in IR")?;
        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[2])
            .ok_or("run_difftest: no CargoPackage in IR")?;

        // Materialize the Rust package with cdylib added, then build it.
        let rust_root = tempfile::tempdir()?;
        cargo_package.materialize(rust_root.path())?;
        let mut cargo_toml = CargoToml::open(&rust_root.path().join("Cargo.toml"))?;
        cargo_toml.add_workspace();
        cargo_toml.ensure_cdylib();
        cargo_toml.save()?;

        info!("Building Rust package as cdylib for diff testing...");
        let status = Command::new("cargo")
            .args(["build", "--release", "--lib"])
            .current_dir(rust_root.path())
            .status()
            .map_err(|e| format!("run_difftest: failed to run cargo build: {e}"))?;
        if !status.success() {
            return Err("run_difftest: cargo build --release --lib failed".into());
        }

        let rust_so = find_so_in_dir(&rust_root.path().join("target/release"))
            .ok_or("run_difftest: no .so found in target/release")?;

        // Write difftest_suite.c and compile it.
        let difftest_root = tempfile::tempdir()?;
        let suite_path = difftest_root.path().join("difftest_suite.c");
        let bin_path = difftest_root.path().join("difftest_bin");
        std::fs::write(&suite_path, diff_test_suite.source.as_bytes())?;

        info!("Compiling difftest_suite.c...");
        let status = Command::new("clang")
            .arg(&suite_path)
            .arg("-o")
            .arg(&bin_path)
            .arg("-ldl")
            .status()
            .map_err(|e| format!("run_difftest: failed to run clang: {e}"))?;
        if !status.success() {
            return Err("run_difftest: clang failed to compile difftest_suite.c".into());
        }

        // Run the diff test binary.
        info!("Running diff test binary...");
        let output = Command::new(&bin_path)
            .arg(&c_library.so_path)
            .arg(&rust_so)
            .output()
            .map_err(|e| format!("run_difftest: failed to run difftest_bin: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_output(&stdout))
    }
}

fn parse_output(output: &str) -> Box<DiffTestResult> {
    let mut passed = 0usize;
    let mut failures = Vec::new();

    for line in output.lines() {
        if line.starts_with("PASS ") {
            passed += 1;
        } else if line.starts_with("FAIL ") || line.starts_with("WARN ") {
            failures.push(line.to_string());
        }
    }

    let failed = failures.len();
    let total = passed + failed;
    info!("Diff test result: {passed}/{total} passed");

    Box::new(DiffTestResult { passed, failed, total, failures })
}

/// Returns the first `.so` file found directly in `dir` (non-recursive, skips subdirectories).
fn find_so_in_dir(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("so") {
            return Some(path);
        }
    }
    None
}
