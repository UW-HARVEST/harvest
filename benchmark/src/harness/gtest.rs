//! GoogleTest-based validation for translated shared libraries.
//!
//! A test case may ship a `gtest_suite/` directory: a CMake project containing
//! GoogleTest tests that call the library's exported C-ABI symbols directly.
//! Unlike the cando2 runner (single dispatch symbol, one call per vector),
//! a gtest suite can exercise multi-call API sequences and stateful setups.
//!
//! # Contract with the suite
//! - `gtest_suite/CMakeLists.txt` accepts `-DTEST_LIB_PATH=<abs .so>` (library
//!   under test) and is otherwise self-contained: it declares its own
//!   tag-pinned GoogleTest via FetchContent.
//! - The test executable target is named `harvest_gtest`.
//!
//! # Execution model
//! Tests are enumerated with `--gtest_list_tests` and then each test runs in
//! its own process via `--gtest_filter=<name>`. This mirrors the existing
//! one-runner-process-per-vector model and keeps a crashing test (e.g. a
//! segfault inside the translated library) from taking down the results of
//! the remaining tests — gtest itself writes no report at all if the process
//! dies mid-run.

use crate::error::HarvestResult;
use crate::harness::library;
use crate::stats::TestResult;
use harvest_core::cargo_utils::{self, CargoToml};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Directory (inside a test case and inside the translated output) holding the suite
pub const GTEST_SUITE_DIR: &str = "gtest_suite";

/// Where the suite is built, relative to the translated output directory
const GTEST_BUILD_SUBDIR: &str = "target/gtest_build";

/// Required name of the suite's test executable target
const GTEST_BINARY_NAME: &str = "harvest_gtest";

/// Environment variable for shared library search paths
#[cfg(target_os = "macos")]
const LD_LIBRARY_PATH_ENV: &str = "DYLD_LIBRARY_PATH";
#[cfg(target_os = "windows")]
const LD_LIBRARY_PATH_ENV: &str = "PATH";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const LD_LIBRARY_PATH_ENV: &str = "LD_LIBRARY_PATH";

/// Validates a translated Rust library by building and running the test case's
/// GoogleTest suite against the compiled cdylib.
///
/// # Arguments
/// * `program_name` - Name of the program being tested
/// * `input_dir` - Directory containing the original test case (source of
///   `gtest_suite/`; equal to `output_dir` in test-only reruns)
/// * `output_dir` - Directory containing the translated Rust project
/// * `timeout` - Timeout in seconds for each individual test
///
/// # Returns
/// Tuple of (test_results, error_messages). One `TestResult` per gtest test,
/// with `filename` set to the full `Suite.Test` name.
pub fn run_gtest_validation(
    program_name: &str,
    input_dir: &Path,
    output_dir: &Path,
    timeout: u64,
) -> HarvestResult<(Vec<TestResult>, Vec<String>)> {
    // Copy the suite from the original test case unless this is a test-only
    // rerun of an already-translated output directory.
    let suite_dir = output_dir.join(GTEST_SUITE_DIR);
    if input_dir != output_dir {
        cargo_utils::copy_directory_recursive(&input_dir.join(GTEST_SUITE_DIR), &suite_dir)
            .map_err(|e| format!("Failed to copy gtest_suite directory: {}", e))?;
    }
    if !suite_dir.is_dir() {
        return Err(format!("gtest_suite directory not found at {}", suite_dir.display()).into());
    }

    // Rebuild the translated project as a cdylib (same preparation as library
    // validation).
    let mut cargo = CargoToml::open(&output_dir.join("Cargo.toml"))?;
    cargo.add_workspace();
    cargo.ensure_cdylib();
    cargo.save()?;

    log::info!("Rebuilding project as cdylib...");
    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .current_dir(output_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run cargo build: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cargo build --release failed: {}", stderr).into());
    }
    log::info!("✅ Cdylib build completed successfully");

    let lib_path = library::locate_compiled_library(output_dir, program_name)?;
    let lib_path = lib_path.canonicalize().unwrap_or(lib_path);
    log::info!("Located library at: {}", lib_path.display());

    let gtest_bin = build_gtest_suite(output_dir, &suite_dir, &lib_path)?;
    log::info!("GoogleTest suite built at: {}", gtest_bin.display());

    let ld_library_path = lib_path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let test_names = list_gtest_tests(&gtest_bin, &ld_library_path)?;
    log::info!("Discovered {} GoogleTest test(s)", test_names.len());

    run_gtest_tests(&gtest_bin, &ld_library_path, &test_names, timeout)
}

/// Configures and builds the gtest suite, returning the test binary path.
fn build_gtest_suite(
    output_dir: &Path,
    suite_dir: &Path,
    lib_path: &Path,
) -> HarvestResult<PathBuf> {
    let build_dir = output_dir.join(GTEST_BUILD_SUBDIR);
    fs::create_dir_all(&build_dir)?;

    log::info!("Configuring GoogleTest suite...");
    let output = Command::new("cmake")
        .arg("-S")
        .arg(suite_dir)
        .arg("-B")
        .arg(&build_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg(format!("-DTEST_LIB_PATH={}", lib_path.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run cmake configure: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "cmake configure failed for {}:\n{}",
            suite_dir.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    log::info!("Building GoogleTest suite...");
    let output = Command::new("cmake")
        .arg("--build")
        .arg(&build_dir)
        .arg("--parallel")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run cmake build: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "GoogleTest suite build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let gtest_bin = build_dir.join(GTEST_BINARY_NAME);
    if !gtest_bin.exists() {
        return Err(format!(
            "GoogleTest binary not found at {} (the suite must define an executable target named '{}')",
            gtest_bin.display(),
            GTEST_BINARY_NAME
        )
        .into());
    }
    Ok(gtest_bin.canonicalize().unwrap_or(gtest_bin))
}

/// Enumerates test names via `--gtest_list_tests`.
///
/// Listing format: suite lines start at column 0 and end with `.`; test lines
/// are indented. Both may carry trailing `# TypeParam/GetParam` comments.
fn list_gtest_tests(gtest_bin: &Path, ld_library_path: &str) -> HarvestResult<Vec<String>> {
    let output = Command::new(gtest_bin)
        .arg("--gtest_list_tests")
        .env(LD_LIBRARY_PATH_ENV, ld_library_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to list gtest tests: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "--gtest_list_tests failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tests = Vec::new();
    let mut suite = String::new();
    for line in stdout.lines() {
        let entry = line.split('#').next().unwrap_or("");
        if entry.trim().is_empty() {
            continue;
        }
        if !entry.starts_with(' ') {
            // Suite lines end with '.'; skip any other preamble (e.g. the
            // "Running main() from gtest_main.cc" banner).
            if entry.trim_end().ends_with('.') {
                suite = entry.trim().to_string();
            }
        } else if !suite.is_empty() {
            tests.push(format!("{}{}", suite, entry.trim()));
        }
    }

    if tests.is_empty() {
        return Err("gtest suite lists no tests".to_string().into());
    }
    Ok(tests)
}

/// Runs each test in its own process and collects results.
fn run_gtest_tests(
    gtest_bin: &Path,
    ld_library_path: &str,
    test_names: &[String],
    timeout: u64,
) -> HarvestResult<(Vec<TestResult>, Vec<String>)> {
    let mut test_results = Vec::new();
    let mut error_messages = Vec::new();
    let timeout_duration = Duration::from_secs(timeout);

    log::info!("Validating library outputs against GoogleTest suite...");

    for (i, name) in test_names.iter().enumerate() {
        log::info!(
            "Running gtest {} ({} of {})...",
            name,
            i + 1,
            test_names.len()
        );

        match run_single_gtest(gtest_bin, ld_library_path, name, timeout_duration) {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let skipped = stdout.contains("[  SKIPPED ]");
                if output.status.success() {
                    test_results.push(TestResult {
                        filename: name.clone(),
                        passed: true,
                        skipped,
                    });
                    if skipped {
                        log::info!("Skipping gtest {} (GTEST_SKIP)", name);
                    } else {
                        log::info!("✅ Test {} passed", name);
                    }
                } else {
                    test_results.push(TestResult {
                        filename: name.clone(),
                        passed: false,
                        skipped: false,
                    });
                    let error = format!(
                        "gtest {} failed: status {:?}\nstdout:\n{}\nstderr:\n{}",
                        name,
                        output.status.code(),
                        stdout,
                        String::from_utf8_lossy(&output.stderr)
                    );
                    error_messages.push(error.clone());
                    log::info!("❌ Test {} failed", name);
                }
            }
            Err(e) => {
                test_results.push(TestResult {
                    filename: name.clone(),
                    passed: false,
                    skipped: false,
                });
                let error = format!("gtest {} failed: {}", name, e);
                error_messages.push(error.clone());
                log::info!("❌ {}", error);
            }
        }
    }

    Ok((test_results, error_messages))
}

/// Runs a single test in its own process via `--gtest_filter`.
fn run_single_gtest(
    gtest_bin: &Path,
    ld_library_path: &str,
    test_name: &str,
    timeout: Duration,
) -> HarvestResult<Output> {
    let mut child = Command::new(gtest_bin)
        .arg(format!("--gtest_filter={}", test_name))
        .env(LD_LIBRARY_PATH_ENV, ld_library_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn gtest binary: {}", e))?;

    match child.wait_timeout(timeout) {
        Ok(Some(_)) => child
            .wait_with_output()
            .map_err(|e| format!("Failed to read gtest output: {}", e).into()),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("Test timed out after {} seconds", timeout.as_secs()).into())
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("Error waiting for gtest: {}", e).into())
        }
    }
}
