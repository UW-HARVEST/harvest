//! Auto-fix tool for Rust projects with compilation errors
//!
//! This tool automatically fixes compilation errors by:
//! 1. Compiling the project
//! 2. Classifying errors by file using LLM
//! 3. Fixing each file individually using LLM
//! 4. Repeating until success or max iterations

pub mod compiler;
pub mod error_classifier;
pub mod file_fixer;
pub mod version_manager;

use chrono::{DateTime, Utc};
use harvest_core::llm::LLMConfig;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info, warn};

pub use compiler::{BuildResult, compile_project};
pub use error_classifier::{
    DiagLevel, Diagnostic, ErrorClassification, FileErrorReport, classify_errors,
};
pub use file_fixer::fix_file;
pub use version_manager::{
    WorkingDirectory, save_file_version, save_initial_versions, save_iteration_snapshot,
};

/// Configuration for the auto-fix tool
#[derive(Debug, Clone)]
pub struct FixConfig {
    pub llm_config: Arc<LLMConfig>,
    pub max_iterations: usize,
    pub verbose: bool,
    pub parallel: bool,
    pub parallelism: usize,
}

/// Summary of the fixing process
#[derive(Debug, Serialize, Deserialize)]
pub struct FixSummary {
    pub project_name: String,
    pub total_iterations: usize,
    pub final_success: bool,
    pub initial_error_count: usize,
    pub final_error_count: usize,
    pub files_modified: Vec<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub iterations: Vec<IterationRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IterationRecord {
    pub iteration: usize,
    pub errors_before: usize,
    pub errors_after: usize,
    pub files_fixed: Vec<String>,
    pub classification_success: bool,
}

/// Initialize the working directory by copying input to output
pub fn initialize_working_directory(
    input_dir: &Path,
    output_dir: &Path,
) -> Result<WorkingDirectory, Box<dyn std::error::Error>> {
    info!("Initializing working directory");
    info!("  Input:  {}", input_dir.display());
    info!("  Output: {}", output_dir.display());

    // Create output directory
    if output_dir.exists() {
        warn!("Output directory already exists, removing it");
        std::fs::remove_dir_all(output_dir)?;
    }
    std::fs::create_dir_all(output_dir)?;

    // Copy contents of input directory to output
    let mut options = fs_extra::dir::CopyOptions::new();
    options.content_only = true; // Copy only the contents, not the directory itself
    options.overwrite = true;
    fs_extra::dir::copy(input_dir, output_dir, &options)?;

    // Create history directory
    let history_dir = output_dir.join(".fix_history");
    std::fs::create_dir_all(&history_dir)?;

    // Create iteration_all directory for version comparison
    let iteration_all_dir = history_dir.join("iteration_all");
    std::fs::create_dir_all(&iteration_all_dir)?;

    // Create log file
    let log_file = history_dir.join("fix_log.jsonl");
    std::fs::write(&log_file, "")?;

    Ok(WorkingDirectory {
        root: output_dir.to_path_buf(),
        history_dir,
        log_file,
        iteration_all_dir,
    })
}

/// Main auto-fix loop
pub fn auto_fix_project(
    working_dir: &WorkingDirectory,
    config: &FixConfig,
) -> Result<FixSummary, Box<dyn std::error::Error>> {
    let start_time = Utc::now();
    let project_name = working_dir
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    info!("=== Starting Auto-Fix for {} ===", project_name);

    // Save initial versions of all source files
    info!("Saving initial file versions...");
    save_initial_versions(working_dir)?;

    // Initial compilation to get baseline
    info!("Running initial compilation...");
    let initial_result = compile_project(&working_dir.root)?;
    let initial_error_count = initial_result.error_count;

    if initial_result.success {
        info!("✅ Project already compiles successfully!");
        return Ok(FixSummary {
            project_name,
            total_iterations: 0,
            final_success: true,
            initial_error_count: 0,
            final_error_count: 0,
            files_modified: vec![],
            start_time,
            end_time: Utc::now(),
            iterations: vec![],
        });
    }

    info!(
        "Initial compilation: {} errors, {} warnings",
        initial_result.error_count, initial_result.warning_count
    );

    let mut iterations = Vec::new();
    let mut files_modified = Vec::new();
    let mut current_result = initial_result;

    for iteration in 0..config.max_iterations {
        info!(
            "\n=== Iteration {}/{} ===",
            iteration + 1,
            config.max_iterations
        );

        let errors_before = current_result.error_count;

        // If no errors (only warnings), stop iterating
        if errors_before == 0 {
            info!(
                "No errors remaining (only {} warnings), stopping",
                current_result.warning_count
            );
            break;
        }

        // Save snapshot before fixing
        save_iteration_snapshot(&working_dir, iteration, &current_result)?;

        // Classify errors (local parse — no LLM call)
        info!("Classifying errors...");
        let iteration_dir = working_dir
            .history_dir
            .join(format!("iteration_{}", iteration));
        let classification = classify_errors(&current_result);

        if classification.files.is_empty() {
            warn!("No fixable errors identified");
            break;
        }

        info!(
            "=== Files to fix in this iteration: {} ({} total errors) ===",
            classification.files.len(),
            classification.total_errors
        );
        for (idx, file) in classification.files.iter().enumerate() {
            info!(
                "  {}. {} ({} errors, {} warnings)",
                idx + 1,
                file.file_path,
                file.error_count,
                file.warning_count
            );
        }
        info!("");

        // Save classification as JSON for post-mortem inspection.
        if let Ok(json) = serde_json::to_string_pretty(&classification) {
            let _ = std::fs::create_dir_all(&iteration_dir);
            let _ = std::fs::write(iteration_dir.join("classification.json"), json);
        }

        // Fix files in priority order (parallel or sequential)
        let fix_results: Vec<(String, bool)> = if config.parallel {
            info!(
                "Fixing files in parallel (max {} threads)...",
                config.parallelism
            );

            // Build thread pool with limited parallelism
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(config.parallelism)
                .build()
                .expect("Failed to build thread pool");

            pool.install(|| {
                classification
                    .files
                    .par_iter()
                    .map(|file_report| {
                        info!(
                            "Fixing {} ({} errors)",
                            file_report.file_path, file_report.error_count
                        );

                        let success = match fix_file(
                            &working_dir.root,
                            &file_report.file_path,
                            &file_report.errors_text,
                            &config.llm_config,
                        ) {
                            Ok(_) => {
                                info!("  ✓ Fixed {}", file_report.file_path);

                                // Save versioned copy to iteration_all (use iteration+1 since 0 is the initial version)
                                if let Err(e) = save_file_version(
                                    working_dir,
                                    &file_report.file_path,
                                    iteration + 1,
                                ) {
                                    warn!("  Failed to save file version: {}", e);
                                }

                                true
                            }
                            Err(e) => {
                                error!("  ✗ Failed to fix {}: {}", file_report.file_path, e);
                                false
                            }
                        };

                        (file_report.file_path.clone(), success)
                    })
                    .collect()
            })
        } else {
            info!("Fixing files sequentially...");
            classification
                .files
                .iter()
                .map(|file_report| {
                    info!(
                        "Fixing {} ({} errors)",
                        file_report.file_path, file_report.error_count
                    );

                    let success = match fix_file(
                        &working_dir.root,
                        &file_report.file_path,
                        &file_report.errors_text,
                        &config.llm_config,
                    ) {
                        Ok(_) => {
                            info!("  ✓ Fixed {}", file_report.file_path);

                            // Save versioned copy to iteration_all (use iteration+1 since 0 is the initial version)
                            if let Err(e) = save_file_version(
                                working_dir,
                                &file_report.file_path,
                                iteration + 1,
                            ) {
                                warn!("  Failed to save file version: {}", e);
                            }

                            true
                        }
                        Err(e) => {
                            error!("  ✗ Failed to fix {}: {}", file_report.file_path, e);
                            false
                        }
                    };

                    (file_report.file_path.clone(), success)
                })
                .collect()
        };

        // Collect successfully fixed files
        let mut files_fixed_this_iteration = Vec::new();
        for (file_path, success) in fix_results {
            if success {
                files_fixed_this_iteration.push(file_path.clone());
                if !files_modified.contains(&file_path) {
                    files_modified.push(file_path);
                }
            }
        }

        if files_fixed_this_iteration.is_empty() {
            warn!("No files were successfully fixed in this iteration");
            iterations.push(IterationRecord {
                iteration,
                errors_before,
                errors_after: current_result.error_count,
                files_fixed: files_fixed_this_iteration,
                classification_success: true,
            });
            break;
        }

        // Recompile to check progress
        info!("Recompiling after fixes...");
        current_result = compile_project(&working_dir.root)?;

        let record = IterationRecord {
            iteration,
            errors_before,
            errors_after: current_result.error_count,
            files_fixed: files_fixed_this_iteration,
            classification_success: true,
        };
        iterations.push(record);

        if current_result.success {
            info!("✅ Build succeeded!");
            break;
        }

        info!(
            "After iteration {}: {} errors, {} warnings",
            iteration + 1,
            current_result.error_count,
            current_result.warning_count
        );
    }

    let end_time = Utc::now();
    let summary = FixSummary {
        project_name,
        total_iterations: iterations.len(),
        final_success: current_result.success,
        initial_error_count,
        final_error_count: current_result.error_count,
        files_modified,
        start_time,
        end_time,
        iterations,
    };

    // Save final summary
    let summary_path = working_dir.history_dir.join("summary.json");
    let summary_json = serde_json::to_string_pretty(&summary)?;
    std::fs::write(summary_path, summary_json)?;

    Ok(summary)
}
