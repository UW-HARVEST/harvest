use build_c_artifact::CArtifact;
use full_source::CargoPackage;
use generate_exec_difftests::{ExecTestInputs, TestInput};
use harvest_core::cargo_utils::CargoToml;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use run_difftest::DiffTestResult;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tracing::info;

const EXEC_TIMEOUT_SECS: u64 = 10;

pub struct RunExecDiffTest;

impl Tool for RunExecDiffTest {
    fn name(&self) -> &'static str {
        "run_exec_difftest"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let exec_test_inputs = context
            .ir_snapshot
            .get::<ExecTestInputs>(inputs[0])
            .ok_or("run_exec_difftest: no ExecTestInputs in IR")?;
        let c_artifact = context
            .ir_snapshot
            .get::<CArtifact>(inputs[1])
            .ok_or("run_exec_difftest: no CArtifact in IR")?;
        let cargo_package = context
            .ir_snapshot
            .get::<CargoPackage>(inputs[2])
            .ok_or("run_exec_difftest: no CargoPackage in IR")?;

        let Some(ref c_exe) = c_artifact.exe_path else {
            info!("run_exec_difftest: project is not an executable; returning empty result");
            return Ok(empty());
        };
        let c_exe = c_exe.clone();

        if exec_test_inputs.cases.is_empty() {
            info!("run_exec_difftest: no test inputs; returning empty result");
            return Ok(empty());
        }

        // Materialize and build the Rust executable.
        let rust_root = tempfile::tempdir()?;
        cargo_package.materialize(rust_root.path())?;
        let mut cargo_toml = CargoToml::open(&rust_root.path().join("Cargo.toml"))?;
        cargo_toml.add_workspace();
        cargo_toml.save()?;

        info!("run_exec_difftest: building Rust executable...");
        let status = Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(rust_root.path())
            .status()
            .map_err(|e| format!("run_exec_difftest: failed to run cargo build: {e}"))?;
        if !status.success() {
            return Err("run_exec_difftest: cargo build --release failed".into());
        }

        let rust_exe = find_exe_in_dir(&rust_root.path().join("target/release"))
            .ok_or("run_exec_difftest: no executable found in target/release")?;

        // Run each test case against both binaries and compare outputs.
        let timeout = Duration::from_secs(EXEC_TIMEOUT_SECS);
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for (i, input) in exec_test_inputs.cases.iter().enumerate() {
            let test_id = format!("T{:03}", i + 1);
            let stdin = if input.stdin.is_empty() {
                None
            } else {
                Some(input.stdin.as_str())
            };

            let c_out = match exec_runner::run_with_timeout(&c_exe, &input.argv, stdin, timeout) {
                Ok(o) => o,
                Err(e) => {
                    info!("run_exec_difftest: {test_id} C run failed: {e}; skipping");
                    continue;
                }
            };
            let rust_out =
                match exec_runner::run_with_timeout(&rust_exe, &input.argv, stdin, timeout) {
                    Ok(o) => o,
                    Err(e) => {
                        info!("run_exec_difftest: {test_id} Rust run failed: {e}; skipping");
                        continue;
                    }
                };

            let ctx = format_context(input);
            let mut case_failures: Vec<String> = Vec::new();

            if c_out.stdout != rust_out.stdout {
                case_failures.push(format!("FAIL exec_{test_id} {ctx} field=stdout"));
            }
            if c_out.stderr != rust_out.stderr {
                case_failures.push(format!("FAIL exec_{test_id} {ctx} field=stderr"));
            }
            let c_code = c_out.status.code().unwrap_or(-1);
            let rust_code = rust_out.status.code().unwrap_or(-1);
            if c_code != rust_code {
                case_failures.push(format!(
                    "FAIL exec_{test_id} {ctx} field=exit_code c={c_code} rust={rust_code}"
                ));
            }

            if case_failures.is_empty() {
                info!("run_exec_difftest: PASS exec_{test_id}");
                passed += 1;
            } else {
                info!(
                    "run_exec_difftest: FAIL exec_{test_id} ({} field(s))",
                    case_failures.len()
                );
                failed += 1;
                failures.extend(case_failures);
            }
        }

        let total = passed + failed;
        info!("run_exec_difftest: {passed}/{total} test cases passed");

        Ok(Box::new(DiffTestResult {
            passed,
            failed,
            total,
            failures,
        }))
    }
}

fn format_context(input: &TestInput) -> String {
    let argv_str = input.argv.join(",");
    let stdin_preview: String = input.stdin.chars().take(40).collect();
    let ellipsis = if input.stdin.chars().count() > 40 {
        "..."
    } else {
        ""
    };
    format!("argv=[{argv_str}] stdin=\"{stdin_preview}{ellipsis}\"")
}

/// Returns the first regular file with the execute bit set and no extension found
/// directly in `dir` (non-recursive).
fn find_exe_in_dir(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_file()
            && path.extension().is_none()
            && let Ok(meta) = std::fs::metadata(&path)
            && meta.permissions().mode() & 0o111 != 0
        {
            return Some(path);
        }
    }
    None
}

fn empty() -> Box<DiffTestResult> {
    Box::new(DiffTestResult {
        passed: 0,
        failed: 0,
        total: 0,
        failures: vec![],
    })
}
