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
use crate::harness::feature_combo::{enumerate_combos, FeatureCombo, FeatureCombos};
use crate::harness::{
    cleanup_benchmarks, parse_benchmark_dir, parse_test_vectors, validate_binary_output,
};
use crate::io::{
    collect_program_dirs, ensure_output_directory, log_failing_programs, log_found_programs,
    log_summary_stats, validate_input_directory, write_csv_results, write_error_file,
};
use crate::ir_utils::{
    all_cargo_packages, cargo_build_result, raw_cargo_package, raw_source, write_output_result,
};
use crate::logger::TeeLogger;
use crate::stats::{ComboResult, ProgramEvalStats, SummaryStats, TestResult};
use clap::Parser;
use harvest_core::utils::get_version;
use harvest_core::HarvestIR;
use harvest_translate::{transpile, util::set_user_only_umask};
use regex::Regex;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// Encapsulate important results from transpilation
pub struct TranspilationResult {
    translation_success: bool,
    build_success: bool,
    rust_binary_path: Option<PathBuf>,
    build_error: Option<String>,
}

impl TranspilationResult {
    /// Extract relevant info from HarvestIR
    pub fn from_ir(ir: &HarvestIR) -> Self {
        let translation_success = raw_cargo_package(ir).is_ok();
        let (build_success, rust_binary_path, build_error) = match cargo_build_result(ir) {
            Ok(result) if result.success => {
                let first = write_output_result(ir)
                    .ok()
                    .and_then(|r| r.executable.clone());
                (true, first, None)
            }
            Ok(result) => (false, None, Some(result.err.clone())),
            Err(err) => (false, None, Some(err)),
        };

        Self {
            translation_success,
            build_success,
            rust_binary_path,
            build_error,
        }
    }
}

/// Saves each intermediate CargoPackage produced during translation/repair into
/// `{output_dir}/intermediate_builds/pass_N/` for diagnostic inspection.
fn save_intermediate_builds(ir: &HarvestIR, output_dir: &Path) {
    let packages = all_cargo_packages(ir);
    if packages.is_empty() {
        return;
    }
    let intermediate_dir = output_dir.join("intermediate_builds");
    for (i, raw_dir) in packages.iter().enumerate() {
        let pass_dir = intermediate_dir.join(format!("pass_{}", i));
        if let Err(e) = raw_dir.materialize(&pass_dir) {
            log::warn!("Failed to save intermediate build pass {}: {}", i, e);
        }
    }
}

/// Translates a C source directory to a Rust Cargo project using harvest_translate
pub fn translate_c_directory_to_rust_project(
    input_dir: &Path,
    output_dir: &Path,
    config_overrides: &[String],
    modular: bool,
    agentic: bool,
    agentic_verify: bool,
    repair_passes: usize,
) -> TranspilationResult {
    let args: Arc<harvest_translate::cli::Args> = harvest_translate::cli::Args {
        input: Some(input_dir.to_path_buf()),
        output: Some(output_dir.to_path_buf()),
        print_config_path: false,
        config: config_overrides.to_vec(),
        force: false,
        modular,
        agentic,
        agentic_verify,
        agentic_agent: None,
        repair_passes,
        diff_repair_passes: 0,
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
    match transpile(config.into()) {
        Ok(ir) => {
            match raw_source(&ir) {
                Ok(raw_c_source) => {
                    if let Err(e) = raw_c_source.materialize(output_dir.join("c_src")) {
                        log::warn!("Failed to materialize C source: {}", e);
                    }
                }
                Err(e) => log::warn!("Failed to retrieve raw C source from IR: {}", e),
            }
            save_intermediate_builds(&ir, output_dir);
            TranspilationResult::from_ir(&ir)
        }
        Err(e) => {
            log::error!("Failed to transpile (full error): {:#?}", e);
            TranspilationResult {
                translation_success: false,
                build_success: false,
                rust_binary_path: None,
                build_error: Some(format!("Failed to transpile: {}", e)),
            }
        }
    }
}

/// Run all benchmarks for a list of programs
#[allow(clippy::too_many_arguments)]
pub fn run_all_benchmarks(
    program_dirs: &[PathBuf],
    output_dir: &Path,
    config_overrides: &[String],
    timeout: u64,
    modular: bool,
    agentic: bool,
    agentic_verify: bool,
    repair_passes: usize,
    feature_combos: &FeatureCombos,
) -> HarvestResult<Vec<ProgramEvalStats>> {
    // Process all examples
    let mut results = Vec::new();
    let total_examples = program_dirs.len();

    for (i, program_dir) in program_dirs.iter().enumerate() {
        log::error!("\n{}", "=".repeat(80));
        log::info!("Processing example {} of {}", i + 1, total_examples);
        log::info!("{}", "=".repeat(80));

        let result = benchmark_single_program(
            program_dir,
            output_dir,
            config_overrides,
            timeout,
            modular,
            agentic,
            agentic_verify,
            repair_passes,
            feature_combos,
        );

        results.push(result);
    }

    Ok(results)
}

/// Build the translated crate with a specific feature combination and return the
/// path to the compiled binary (for executable crates).
///
/// When `combo.no_default_features` is `false` (the `default` combo), a plain
/// `cargo build --release` is run -- identical to the pre-feature-combo behavior.
/// When `no_default_features` is `true`, `--no-default-features --features=...`
/// is appended so only the explicitly listed features are active.
///
/// Returns `None` (and logs a warning) for library crates -- callers must handle
/// the library path differently.
fn build_with_features(project_dir: &Path, combo: &FeatureCombo) -> HarvestResult<Option<PathBuf>> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .current_dir(project_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if combo.no_default_features {
        cmd.arg("--no-default-features");
        if !combo.features.is_empty() {
            cmd.arg("--features").arg(combo.features.join(","));
        }
    }

    log::info!(
        "Building combo '{}' in {}",
        combo.label,
        project_dir.display()
    );

    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn cargo build: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cargo build failed for combo '{}': {}", combo.label, stderr).into());
    }

    // Find the binary produced by this build.  We look for a single executable
    // in target/release/ (excluding .d / .rlib / subdirectories).
    let release_dir = project_dir.join("target").join("release");
    let bin = find_release_binary(&release_dir);
    Ok(bin)
}

/// Find the executable binary in a `target/release/` directory.
/// Returns the first file that looks like a plain executable (not a dep file,
/// not a subdirectory, not a `.d`/`.rlib` file).
fn find_release_binary(release_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(release_dir).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if !path.is_file() {
                return None;
            }
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            // Skip known non-binary extensions.
            if matches!(ext, "d" | "rlib" | "rmeta" | "pdb" | "exp" | "lib") {
                return None;
            }
            // Skip files whose stem starts with a dot.
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem.starts_with('.') {
                return None;
            }
            Some(path)
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// Run list of tests and output result/errors
fn run_test_validation(
    binary_path: &Path,
    test_cases: &[crate::harness::TestCase],
    timeout: u64,
    output_dir: &Path,
) -> (Vec<TestResult>, Vec<String>) {
    let mut test_results = Vec::new();
    let mut error_messages = Vec::new();

    log::info!("Validating Rust binary outputs against test cases...");

    for (i, test_case) in test_cases.iter().enumerate() {
        if test_case.has_ub.is_some() {
            log::info!(
                "Skipping test case {} ({} of {})",
                test_case.filename,
                i + 1,
                test_cases.len()
            );
            test_results.push(TestResult {
                filename: test_case.filename.clone(),
                passed: true,
                skipped: true,
            });
            continue;
        }
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
                test_results.push(TestResult {
                    filename: test_case.filename.clone(),
                    passed: true,
                    skipped: false,
                });
                log::info!("✅ Test case {} passed", test_case.filename);
            }
            Err(e) => {
                test_results.push(TestResult {
                    filename: test_case.filename.clone(),
                    passed: false,
                    skipped: false,
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

    (test_results, error_messages)
}

/// Run all benchmarks for a single program, including feature-combo iteration.
#[allow(clippy::too_many_arguments)]
fn benchmark_single_program(
    program_dir: &Path,
    output_root_dir: &Path,
    config_overrides: &[String],
    timeout: u64,
    modular: bool,
    agentic: bool,
    agentic_verify: bool,
    repair_passes: usize,
    feature_combos: &FeatureCombos,
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
    let translation_result = translate_c_directory_to_rust_project(
        &test_case_dir,
        &output_dir,
        config_overrides,
        modular,
        agentic,
        agentic_verify,
        repair_passes,
    );

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

    // For library projects, feature-combo testing is not supported:
    // the library validation flow rebuilds via `cargo build` without feature
    // flags, and per-combo cdylib rebuilds are not wired into the cando2 harness.
    // Library crates are validated once using the default combo.
    if is_lib {
        let (test_results, error_messages) = match harness::library::run_library_validation(
            &program_name,
            program_dir,
            &output_dir,
            &test_cases,
            timeout,
        ) {
            Ok(r) => r,
            Err(e) => {
                let error_msg = format!("Library validation failed: {}", e);
                log::error!("{}", error_msg);
                result.error_message = Some(error_msg);
                return result;
            }
        };

        let passed = test_results
            .iter()
            .filter(|t| t.passed && !t.skipped)
            .count();
        let skipped = test_results.iter().filter(|t| t.skipped).count();

        result.passed_tests = passed;
        result.skipped_tests = skipped;
        result.test_results = test_results;
        result.combo_results.push(ComboResult {
            feature_combo: "default".to_string(),
            combo_passed: result.failed_tests() == 0,
        });

        log_program_summary(&result);

        if !error_messages.is_empty() {
            let error_file_path = output_dir.join("results.err");
            if let Err(e) = write_error_file(&error_file_path, &error_messages) {
                log::info!("Warning: Failed to write error file: {}", e);
            }
        }

        return result;
    }

    // --- Executable path: iterate feature combos ---

    // Enumerate the combos to test.
    let cargo_toml_path = output_dir.join("Cargo.toml");
    let combos = match enumerate_combos(&cargo_toml_path, feature_combos) {
        Ok(c) => c,
        Err(e) => {
            let error_msg = format!("Feature-combo enumeration failed: {}", e);
            log::error!("{}", error_msg);
            result.error_message = Some(error_msg);
            return result;
        }
    };

    log::info!("Feature combos to test: {}", combos.len());

    // For the `default` combo (no_default_features == false), use the binary
    // that was already built by transpile.  For non-default combos, rebuild.
    let initial_binary = translation_result.rust_binary_path;

    let mut all_error_messages: Vec<String> = Vec::new();
    let mut first_combo_test_results: Option<(Vec<TestResult>, usize, usize)> = None;

    for (combo_idx, combo) in combos.iter().enumerate() {
        log::info!(
            "Testing combo {} of {}: '{}'",
            combo_idx + 1,
            combos.len(),
            combo.label
        );

        // Get or build the binary for this combo.
        let binary_path: PathBuf = if !combo.no_default_features {
            // Default combo: reuse the binary produced by transpile.
            match &initial_binary {
                Some(p) if p.exists() => p.clone(),
                other => {
                    let error = format!(
                        "Rust build reported success, but expected output artifact \
                         was not found at {:?}",
                        other
                    );
                    log::error!("{}", error);
                    result.error_message = Some(error);
                    return result;
                }
            }
        } else {
            // Non-default combo: rebuild with the requested features.
            match build_with_features(&output_dir, combo) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    let error = format!(
                        "cargo build succeeded for combo '{}' but no binary was found",
                        combo.label
                    );
                    log::warn!("{}", error);
                    result.combo_results.push(ComboResult {
                        feature_combo: combo.label.clone(),
                        combo_passed: false,
                    });
                    all_error_messages.push(error);
                    continue;
                }
                Err(e) => {
                    let error = format!("Build failed for combo '{}': {}", combo.label, e);
                    log::warn!("{}", error);
                    result.combo_results.push(ComboResult {
                        feature_combo: combo.label.clone(),
                        combo_passed: false,
                    });
                    all_error_messages.push(error);
                    continue;
                }
            }
        };

        let (test_results, error_messages) =
            run_test_validation(&binary_path, &test_cases, timeout, &output_dir);

        let combo_passed =
            error_messages.is_empty() && test_results.iter().all(|t| t.passed || t.skipped);

        result.combo_results.push(ComboResult {
            feature_combo: combo.label.clone(),
            combo_passed,
        });

        // For the primary (default / first) combo, also populate the
        // top-level aggregate stats that `SummaryStats` and logging use.
        if first_combo_test_results.is_none() {
            let passed = test_results
                .iter()
                .filter(|t| t.passed && !t.skipped)
                .count();
            let skipped = test_results.iter().filter(|t| t.skipped).count();
            first_combo_test_results = Some((test_results, passed, skipped));
        }

        all_error_messages.extend(error_messages);
    }

    // Populate top-level aggregate stats from the first combo.
    if let Some((test_results, passed, skipped)) = first_combo_test_results {
        result.passed_tests = passed;
        result.skipped_tests = skipped;
        result.test_results = test_results;
    }

    // Print summary for this example
    log_program_summary(&result);

    // Write error messages to results.err file in the output directory if it was created
    if !all_error_messages.is_empty() {
        let error_file_path = output_dir.join("results.err");
        if let Err(e) = write_error_file(&error_file_path, &all_error_messages) {
            log::info!("Warning: Failed to write error file: {}", e);
        }
    }

    result
}

/// Log the per-program summary line.
fn log_program_summary(result: &ProgramEvalStats) {
    log::info!("\nResults for {}:", result.program_name);
    log::info!(
        "  Translation: {}",
        status_emoji(result.translation_success)
    );
    log::info!("  Rust Build: {}", status_emoji(result.rust_build_success));
    log::info!(
        "  Tests: {}/{} passed ({} skipped, {:.1}%)",
        result.passed_tests,
        result.evaluated_tests(),
        result.skipped_tests,
        result.success_rate()
    );
    if result.combo_results.len() > 1 {
        let all_pass = result.combo_results.iter().all(|c| c.combo_passed);
        log::info!(
            "  Feature combos: {} tested, {} strict aggregate pass",
            result.combo_results.len(),
            if all_pass { "[OK]" } else { "[FAIL]" }
        );
    }
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
    log::info!("Harvest version: {}", get_version());
    run(args)
}

fn apply_regex_filter(
    program_dirs: &mut Vec<PathBuf>,
    pattern: &str,
    keep_matches: bool,
    label: &str,
) -> HarvestResult<()> {
    let regex =
        Regex::new(pattern).map_err(|e| format!("Invalid regex pattern '{}': {}", pattern, e))?;
    let mut removed_names = Vec::new();
    program_dirs.retain(|path| {
        let matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| regex.is_match(name))
            .unwrap_or(false);
        let keep = if keep_matches { matches } else { !matches };
        if !keep {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                removed_names.push(name.to_string());
            }
        }
        keep
    });
    log::info!(
        "{} '{}' applied: {} programs remaining, {} removed",
        label,
        pattern,
        program_dirs.len(),
        removed_names.len(),
    );
    if !removed_names.is_empty() {
        let past_tense = match label {
            "Filter" => "Filtered",
            "Exclude" => "Excluded",
            _ => label,
        };
        log::info!("{}: {}", past_tense, removed_names.join(", "));
    }
    Ok(())
}

fn run(args: Args) -> HarvestResult<()> {
    log::info!("Running Benchmarks");
    log::info!("Input directory: {}", args.input_dir.display());
    log::info!("Output directory: {}", args.output_dir.display());
    log::info!(
        "Using {} Translation",
        if args.modular {
            "Modular"
        } else {
            "All-at-once"
        }
    );

    // Get the programs to evaluate.
    // If the input itself is a single test case root, run just that; otherwise, run children.
    let mut program_dirs = if parse_benchmark_dir(&args.input_dir).is_ok() {
        vec![args.input_dir.clone()]
    } else {
        collect_program_dirs(&args.input_dir)?
    };

    if let Some(filter_pattern) = &args.filter {
        apply_regex_filter(&mut program_dirs, filter_pattern, true, "Filter")?;
    }

    if let Some(exclude_pattern) = &args.exclude {
        apply_regex_filter(&mut program_dirs, exclude_pattern, false, "Exclude")?;
    }

    log_found_programs(&program_dirs, &args.input_dir)?;

    // Process all programs
    let results = run_all_benchmarks(
        &program_dirs,
        &args.output_dir,
        &args.config,
        args.timeout,
        args.modular,
        args.agentic,
        args.agentic_verify,
        args.repair_passes,
        &args.feature_combos,
    )?;
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
