mod cli;
mod error;
mod harness;
mod io;
mod ir_utils;
mod logger;
mod runner;
mod stats;
use crate::cli::Args;
use crate::error::HarvestResult;
use crate::harness::{
    cleanup_benchmarks, parse_benchmark_dir, parse_test_vectors, validate_binary_output,
};
use crate::io::{
    collect_program_dirs, ensure_output_directory, log_failing_programs, log_found_programs,
    log_summary_stats, validate_input_directory, write_csv_results, write_error_file,
};
use crate::ir_utils::{cargo_build_result, raw_cargo_package, raw_source};
use crate::logger::TeeLogger;
use crate::stats::{ProgramEvalStats, SummaryStats, TestResult};
use clap::Parser;
use harvest_core::HarvestIR;
use harvest_translate::{transpile, util::set_user_only_umask};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// Encapsulate important results from transpilation
pub struct TranspilationResult {
    translation_success: bool,
    build_success: bool,
    rust_binary_path: PathBuf,
    build_error: Option<String>,
}

impl TranspilationResult {
    /// Extract relevant info from HarvestIR
    pub fn from_ir(ir: &HarvestIR) -> Self {
        let translation_success = raw_cargo_package(ir).is_ok();
        let (build_success, rust_binary_path, build_error) = match cargo_build_result(ir) {
            Ok(artifacts) => {
                // Prefer the first artifact as the "binary" path for executable cases.
                let first = artifacts.get(0).cloned().unwrap_or_default();
                (true, first, None)
            }
            Err(err) => (false, PathBuf::new(), Some(err.clone())),
        };

        Self {
            translation_success,
            build_success,
            rust_binary_path,
            build_error,
        }
    }
}

/// Translates a C source directory to a Rust Cargo project using harvest_translate
pub fn translate_c_directory_to_rust_project(
    input_dir: &Path,
    output_dir: &Path,
    config_overrides: &[String],
) -> TranspilationResult {
    let args: Arc<harvest_translate::cli::Args> = harvest_translate::cli::Args {
        input: Some(input_dir.to_path_buf()),
        output: Some(output_dir.to_path_buf()),
        print_config_path: false,
        config: config_overrides.to_vec(),
        force: false,
    }
    .into();
    let mut config = harvest_translate::cli::initialize(args).expect("Failed to generate config");
    if config.log_filter.is_empty() {
        config.log_filter = "off".to_owned(); // Disable console logging in harvest_translate
    }
    /*
    TODO: This isn't general anyway, only logs a single tool's parameters

    let tool_config = &config.tools.raw_source_to_cargo_llm;
    log::info!(
        "Translating code using {}:{} with max tokens: {}",
        tool_config.backend,
        tool_config.model,
        tool_config.max_tokens
    );*/
    let ir_result = transpile(config.into());
    let raw_c_source = raw_source(ir_result.as_ref().unwrap()).unwrap();
    raw_c_source
        .materialize(output_dir.join("c_src"))
        .expect("Failed to materialize C source");

    match ir_result {
        Ok(ir) => TranspilationResult::from_ir(&ir),
        Err(_) => TranspilationResult {
            translation_success: false,
            build_success: false,
            rust_binary_path: PathBuf::new(),
            build_error: Some("Failed to transpile".to_string()),
        },
    }
}

/// Run all benchmarks for a list of programs
pub fn run_all_benchmarks(
    program_dirs: &[PathBuf],
    output_dir: &Path,
    config_overrides: &[String],
    timeout: u64,
) -> HarvestResult<Vec<ProgramEvalStats>> {
    // Process all examples
    let mut results = Vec::new();
    let total_examples = program_dirs.len();

    for (i, program_dir) in program_dirs.iter().enumerate() {
        log::error!("\n{}", "=".repeat(80));
        log::info!("Processing example {} of {}", i + 1, total_examples);
        log::info!("{}", "=".repeat(80));

        let result = benchmark_single_program(program_dir, output_dir, config_overrides, timeout);

        results.push(result);
    }

    Ok(results)
}

/// Run list of tests and output result/errors
fn run_test_validation(
    binary_path: &Path,
    test_cases: &[crate::harness::TestCase],
    timeout: u64,
    output_dir: &Path,
) -> (Vec<TestResult>, Vec<String>, usize) {
    let mut test_results = Vec::new();
    let mut error_messages = Vec::new();
    let mut passed_tests = 0;

    log::info!("Validating Rust binary outputs against test cases...");

    for (i, test_case) in test_cases.iter().enumerate() {
        log::info!(
            "Running test case {} ({} of {})...",
            test_case.filename,
            i + 1,
            test_cases.len()
        );

        log::info!(
            "Validating output for test case with args: {:?} stdin: {:?}",
            test_case.argv,
            test_case.stdin,
        );

        let timeout_opt = Some(timeout);
        match validate_binary_output(binary_path, test_case, timeout_opt) {
            Ok(()) => {
                passed_tests += 1;
                test_results.push(TestResult {
                    filename: test_case.filename.clone(),
                    passed: true,
                });
                log::info!("✅ Test case {} passed", test_case.filename);
            }
            Err(e) => {
                test_results.push(TestResult {
                    filename: test_case.filename.clone(),
                    passed: false,
                });
                let error = format!("Test case {} failed: {}", test_case.filename, e);
                error_messages.push(error);
                log::info!("❌ Test case {} failed: {}", test_case.filename, e);
                test_case
                    .write_to_disk(output_dir)
                    .expect("failed to write test case to disk");
            }
        }
    }

    (test_results, error_messages, passed_tests)
}

/// For library cases, copy the built shared object into the candidate directory so the provided
/// cando2 runner can load it, build the runner, and execute each test vector.
fn run_library_validation(
    program_name: &str,
    candidate_dir: &Path,
    translation_result: &TranspilationResult,
    test_cases: &[crate::harness::TestCase],
    timeout: u64,
) -> HarvestResult<(Vec<TestResult>, Vec<String>, usize)> {
    // Locate a shared library artifact built in this output directory.
    let pkg_name = read_package_name(&candidate_dir.join("Cargo.toml"))
        .unwrap_or_else(|| program_name.to_string());
    let target_release = candidate_dir.join("target").join("release");
    let expected_stem = format!("lib{}", pkg_name.replace('-', "_"));
    let libs: Vec<_> = fs::read_dir(&target_release)
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!("Failed to read {}: {}", target_release.display(), e).into()
        })?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| matches!(ext, "so" | "dylib" | "dll"))
                .unwrap_or(false)
        })
        .collect();
    let desired_stem =
        read_runner_library_stem(candidate_dir).unwrap_or_else(|| expected_stem.clone());
    let lib_path = libs
        .iter()
        .find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| stem == desired_stem)
                .unwrap_or(false)
        })
        .cloned()
        .or_else(|| {
            libs.iter()
                .find(|p| {
                    p.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|stem| stem == expected_stem)
                        .unwrap_or(false)
                })
                .cloned()
        })
        .or_else(|| libs.get(0).cloned())
        .ok_or_else(|| {
            format!(
                "No shared library found in {} (looked for stems {} or {})",
                target_release.display(),
                desired_stem,
                expected_stem
            )
        })?;

    // Copy it into CANDIDATE_DIR/translated_rust/target/release/lib<name>.so so the runner finds it.
    let rust_artifacts_dir = candidate_dir
        .join("translated_rust")
        .join("target")
        .join("release");
    std::fs::create_dir_all(&rust_artifacts_dir)?;
    let desired_stem = read_runner_library_stem(candidate_dir)
        .unwrap_or_else(|| format!("lib{}", pkg_name.replace('-', "_")));
    let dest_name = format!(
        "{}.{}",
        desired_stem,
        lib_path.extension().unwrap().to_string_lossy()
    );
    let dest_path = rust_artifacts_dir.join(dest_name);
    std::fs::copy(lib_path, &dest_path).map_err(|e| -> Box<dyn std::error::Error> {
        format!(
            "Failed to copy library artifact to {}: {}",
            dest_path.display(),
            e
        )
        .into()
    })?;

    // Build the provided runner.
    let runner_dir = candidate_dir.join("runner");
    let runner_target_dir = translation_result
        .rust_binary_path
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("runner_build"))
        .unwrap_or_else(|| candidate_dir.join("runner_build"));
    std::fs::create_dir_all(&runner_target_dir)?;
    // Ensure cando2 is available in output and manifests point to it.
    let cando_dst = candidate_dir.join("translated_tools").join("cando2");
    if !cando_dst.exists() {
        let cando_src = find_cando2_source(candidate_dir).ok_or_else(|| {
            "Unable to locate cando2 source (searched ancestors for tools/cando2)".to_string()
        })?;
        copy_optional_dir(&cando_src, &cando_dst)?;
    }
    add_local_workspace_guard(&runner_dir.join("Cargo.toml"))?;
    add_local_workspace_guard(&runner_dir.join("fuzz").join("Cargo.toml"))?;
    let new_dep_path = "../translated_tools/cando2";
    rewrite_cando_dep(&runner_dir.join("Cargo.toml"), new_dep_path)?;
    rewrite_cando_dep(&runner_dir.join("fuzz").join("Cargo.toml"), new_dep_path)?;
    let status = std::process::Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--target-dir")
        .arg(&runner_target_dir)
        .current_dir(&runner_dir)
        .status()
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!("Failed to build runner in {}: {}", runner_dir.display(), e).into()
        })?;
    if !status.success() {
        return Err(format!("Runner build failed with status {:?}", status.code()).into());
    }
    let runner_bin = {
        let pkg_name = read_package_name(&runner_dir.join("Cargo.toml"));
        let candidate = runner_target_dir.join("release");
        if let Some(name) = pkg_name {
            let named = candidate.join(&name);
            if named.exists() {
                named
            } else {
                candidate.join("runner")
            }
        } else {
            candidate.join("runner")
        }
    };
    if !runner_bin.exists() {
        return Err(format!("Runner binary not found at {}", runner_bin.display()).into());
    }

    // Run tests via the runner.
    let mut test_results = Vec::new();
    let mut error_messages = Vec::new();
    let mut passed_tests = 0;
    let timeout = Duration::from_secs(timeout);
    let ld_path = format!(
        "{}:{}",
        rust_artifacts_dir.display(),
        candidate_dir.join("target").join("release").display()
    );

    for (i, test_case) in test_cases.iter().enumerate() {
        log::info!(
            "Running library test case {} ({} of {})...",
            test_case.filename,
            i + 1,
            test_cases.len()
        );
        let mut cmd = std::process::Command::new(&runner_bin);
        cmd.arg("lib")
            .arg("-c")
            .arg(&test_case.filename)
            .current_dir(&runner_dir)
            .env("RUST_ARTIFACTS", "1")
            .env("LD_LIBRARY_PATH", &ld_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| -> Box<dyn std::error::Error> {
            format!("Failed to spawn runner for {}: {}", test_case.filename, e).into()
        })?;

        // Apply timeout.
        use wait_timeout::ChildExt;
        let output = match child.wait_timeout(timeout) {
            Ok(Some(_)) => child
                .wait_with_output()
                .map_err(|e| -> Box<dyn std::error::Error> {
                    format!("Failed to read runner output: {}", e).into()
                })?,
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Runner timed out after {} seconds", timeout.as_secs()).into());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Error waiting for runner: {}", e).into());
            }
        };

        if output.status.success() {
            passed_tests += 1;
            test_results.push(TestResult {
                filename: test_case.filename.clone(),
                passed: true,
            });
        } else {
            test_results.push(TestResult {
                filename: test_case.filename.clone(),
                passed: false,
            });
            let actual_stdout = String::from_utf8_lossy(&output.stdout);
            let actual_stderr = String::from_utf8_lossy(&output.stderr);
            let error = format!(
                "Runner failed for {}: status {:?}\nstdout:\n{}\nstderr:\n{}",
                test_case.filename,
                output.status.code(),
                actual_stdout,
                actual_stderr
            );
            error_messages.push(error);
        }
    }

    Ok((test_results, error_messages, passed_tests))
}

/// Run all benchmarks for a single program
fn benchmark_single_program(
    program_dir: &Path,
    output_root_dir: &Path,
    config_overrides: &[String],
    timeout: u64,
) -> ProgramEvalStats {
    let program_name = program_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut result = ProgramEvalStats::new(&program_name);

    log::info!("Translating program: {}", program_name);
    log::info!("Input directory: {}", program_dir.display());
    let is_lib = program_name.ends_with("_lib");
    log::info!(
        "Detected project type: {}",
        if is_lib { "library" } else { "executable" }
    );

    // Get program output directory
    let output_dir = output_root_dir.join(&program_name);
    log::info!("Output directory: {}", output_dir.display());

    // Check for required subdirectories & log error if we don't find them
    // We use the test_case root (not src/) so translate can see CMakeLists.txt.
    let (test_case_dir, test_vectors_dir) = match parse_benchmark_dir(program_dir) {
        Ok(dirs) => dirs,
        Err(e) => {
            result.error_message = Some(e.to_string());
            return result;
        }
    };

    // Parse test vectors
    let test_cases = match parse_test_vectors(test_vectors_dir) {
        Ok(vectors) => vectors,
        Err(e) => {
            result.error_message = Some(e.to_string());
            return result;
        }
    };

    result.total_tests = test_cases.len();

    // Log test case parsing success
    if !test_cases.is_empty() {
        log::info!("✅ Successfully parsed {} test case(s)", test_cases.len());
    }

    // Do the actual translation
    let translation_result =
        translate_c_directory_to_rust_project(&test_case_dir, &output_dir, config_overrides);

    result.translation_success = translation_result.translation_success;
    result.rust_build_success = translation_result.build_success;

    if translation_result.translation_success {
        log::info!("✅ Translation completed successfully!");
    } else {
        let error = format!(
            "Failed to translate C project: {:?}",
            translation_result.build_error
        );
        result.error_message = Some(error.clone());
        log::info!("❌ Translation failed");
        return result;
    }

    if translation_result.build_success {
        log::info!("✅ Rust build completed successfully!");
    } else {
        let error = format!(
            "Failed to build Rust project: {:?}",
            translation_result.build_error
        );
        result.error_message = Some(error.clone());
        log::info!("❌ Rust build failed");
        return result;
    }

    assert!(translation_result.rust_binary_path.exists());

    let is_lib = program_name.ends_with("_lib");

    // Library and executable validation differ.
    let (test_results, error_messages, passed_tests) = if is_lib {
        // Prevent cargo from attaching to the parent workspace when building generated outputs.
        let _ = add_local_workspace_guard(&output_dir.join("Cargo.toml"));
        // Copy runner and test_vectors for convenience and for cando2 expectations.
        let _ = copy_optional_dir(&program_dir.join("runner"), &output_dir.join("runner"));
        let _ = copy_optional_dir(
            &program_dir.join("test_vectors"),
            &output_dir.join("test_vectors"),
        );
        if let Err(e) = ensure_cdylib(&output_dir) {
            result.error_message = Some(format!("Failed to prepare cdylib: {e}"));
            return result;
        }
        // Rebuild after ensuring cdylib.
        let _ = Command::new("cargo")
            .arg("build")
            .arg("--release")
            .current_dir(&output_dir)
            .status();
        match run_library_validation(
            &program_name,
            &output_dir,
            &translation_result,
            &test_cases,
            timeout,
        ) {
            Ok(r) => r,
            Err(e) => {
                result.error_message = Some(e.to_string());
                return result;
            }
        }
    } else {
        run_test_validation(
            &translation_result.rust_binary_path,
            &test_cases,
            timeout,
            &output_dir,
        )
    };

    result.test_results = test_results;
    result.passed_tests = passed_tests;

    // Print summary for this example
    log::info!("\nResults for {}:", program_name);
    log::info!(
        "  Translation: {}",
        status_emoji(result.translation_success)
    );
    log::info!("  Rust Build: {}", status_emoji(result.rust_build_success));
    log::info!(
        "  Tests: {}/{} passed ({:.1}%)",
        result.passed_tests,
        result.total_tests,
        result.success_rate()
    );

    // Write error messages to results.err file in the output directory if it was created
    if !error_messages.is_empty() {
        let error_file_path = output_dir.join("results.err");
        if let Err(e) = write_error_file(&error_file_path, &error_messages) {
            log::info!("Warning: Failed to write error file: {}", e);
        }
    }

    result
}

fn main() -> HarvestResult<()> {
    set_user_only_umask();
    let args = Args::parse();

    // Validate input directory exists
    validate_input_directory(&args.input_dir)?;

    // Create output directory if it doesn't exist
    ensure_output_directory(&args.output_dir)?;

    let log_file = File::create(args.output_dir.join("output.log"))?;
    TeeLogger::init(log::LevelFilter::Info, log_file)?;
    run(args)
}

fn run(args: Args) -> HarvestResult<()> {
    log::info!("Running Benchmarks");
    log::info!("Input directory: {}", args.input_dir.display());
    log::info!("Output directory: {}", args.output_dir.display());

    // Get the programs to evaluate.
    // If the input itself is a single test case root, run just that; otherwise, run children.
    let mut program_dirs = if parse_benchmark_dir(&args.input_dir).is_ok() {
        vec![args.input_dir.clone()]
    } else {
        collect_program_dirs(&args.input_dir)?
    };

    if args.no_lib {
        let before = program_dirs.len();
        let mut skipped_names = Vec::new();
        program_dirs.retain(|path| {
            let is_lib = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with("_lib"))
                .unwrap_or(false);
            if is_lib {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    skipped_names.push(name.to_string());
                }
                return false;
            }
            true
        });
        let skipped = before.saturating_sub(program_dirs.len());
        log::info!("no-lib flag enabled: skipped {} *_lib programs", skipped);
        if !skipped_names.is_empty() {
            log::info!("Skipped: {}", skipped_names.join(", "));
        }
        log::info!("Remaining programs: {}", program_dirs.len());
    }

    log_found_programs(&program_dirs, &args.input_dir)?;

    // Process all programs
    let results = run_all_benchmarks(&program_dirs, &args.output_dir, &args.config, args.timeout)?;
    let csv_output_path = args.output_dir.join("results.csv");
    write_csv_results(&csv_output_path, &results)?;

    let summary_stats = SummaryStats::from_results(&results);
    log_summary_stats(&summary_stats);

    log::info!("\nOutput Files:");
    log::info!("  Translated projects: {}", args.output_dir.display());
    log::info!("  CSV results: {}", csv_output_path.display());
    log::info!("  Error logs: results.err files in each translated project directory");

    // Print examples with issues
    log_failing_programs(&results);

    log::info!("\nProcessing complete! Check the CSV file and individual project directories for detailed results.");

    cleanup_benchmarks(&results, &args.output_dir);

    Ok(())
}

fn status_emoji(success: bool) -> &'static str {
    match success {
        true => "✅",
        false => "❌",
    }
}

fn copy_optional_dir(src: &Path, dst: &Path) -> HarvestResult<()> {
    if !src.exists() {
        return Ok(());
    }
    fn recurse(src: &Path, dst: &Path) -> HarvestResult<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                recurse(&path, &target)?;
            } else {
                fs::copy(&path, &target)?;
            }
        }
        Ok(())
    }
    recurse(src, dst)
}

/// Ensure the Cargo.toml in the output dir builds a cdylib (required for runner loading).
fn ensure_cdylib(output_dir: &Path) -> HarvestResult<()> {
    let manifest = output_dir.join("Cargo.toml");
    if !manifest.exists() {
        return Err(format!("Cargo.toml not found in {}", output_dir.display()).into());
    }
    let mut contents = fs::read_to_string(&manifest)?;
    if contents.contains("crate-type") {
        return Ok(());
    }
    contents.push_str("\n[lib]\ncrate-type = [\"cdylib\"]\n");
    fs::write(&manifest, contents)?;
    Ok(())
}

/// Extract package name from Cargo.toml (best-effort).
fn read_package_name(manifest: &Path) -> Option<String> {
    let contents = fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            if let Some((_, rest)) = trimmed.split_once('=') {
                let val = rest.trim().trim_matches('"').to_string();
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Rewrite cando2 path dependency in a Cargo.toml to point to a local copied tool.
fn rewrite_cando_dep(manifest: &Path, new_path: &str) -> HarvestResult<()> {
    if !manifest.exists() {
        return Ok(());
    }
    let contents = fs::read_to_string(manifest)?;
    let mut changed = false;
    let mut new_lines = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("cando2") && trimmed.contains("path") {
            new_lines.push(format!("cando2 = {{ path = \"{}\" }}", new_path));
            changed = true;
        } else {
            new_lines.push(line.to_string());
        }
    }
    if changed {
        fs::write(manifest, new_lines.join("\n"))?;
    }
    Ok(())
}

/// Find the cando2 source directory by walking up ancestors from `start` and looking for
/// `tools/cando2/Cargo.toml` or `Test-Corpus/tools/cando2/Cargo.toml`.
fn find_cando2_source(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidates = [
            ancestor.join("tools").join("cando2"),
            ancestor.join("Test-Corpus").join("tools").join("cando2"),
        ];
        for cand in candidates {
            if cand.join("Cargo.toml").exists() {
                return Some(cand);
            }
        }
    }
    None
}

/// Parse runner source to find an explicit `library: "<name>"` stem if provided.
fn read_runner_library_stem(candidate_dir: &Path) -> Option<String> {
    let runner_main = candidate_dir.join("runner").join("src").join("main.rs");
    let contents = fs::read_to_string(&runner_main).ok()?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("library:") {
            if let Some((_, val)) = rest.split_once('"') {
                let name = val.split('"').next().unwrap_or("").trim();
                if !name.is_empty() {
                    return Some(format!("lib{}", name));
                }
            }
        }
    }
    None
}

/// Ensure the given manifest opts out of any parent workspace by declaring an empty workspace.
fn add_local_workspace_guard(manifest: &Path) -> HarvestResult<()> {
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
    fs::write(manifest, updated)?;
    Ok(())
}
