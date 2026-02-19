mod cli;

use auto_fix::{
    auto_fix_project, compile_project, initialize_working_directory, FixConfig,
};
use clap::Parser;
use cli::Cli;
use harvest_core::llm::LLMConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    tools: HashMap<String, serde_json::Value>,
}

fn main() {
    let cli = Cli::parse();

    // Setup logging
    let log_level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    if let Err(e) = run(cli) {
        error!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Validate input
    if !cli.input.exists() {
        return Err(format!("Input directory does not exist: {}", cli.input.display()).into());
    }

    if !cli.input.join("Cargo.toml").exists() {
        return Err(format!("Input directory does not contain Cargo.toml: {}", cli.input.display()).into());
    }

    // Load configuration
    let llm_config = load_llm_config(cli.config.as_deref())?;

    // If analyze-only mode, just compile and classify
    if cli.analyze_only {
        info!("=== Analyze-only mode ===");
        let result = compile_project(&cli.input)?;
        if result.success {
            info!("✅ Project compiles successfully!");
        } else {
            info!("Found {} errors, {} warnings", result.error_count, result.warning_count);
            info!("\nBuild output:\n{}", result.combined_output);
        }
        return Ok(());
    }

    // Initialize working directory
    let working_dir = initialize_working_directory(&cli.input, &cli.output)?;

    info!("Working directory initialized at: {}", working_dir.root.display());
    info!("History will be saved to: {}", working_dir.history_dir.display());

    // Run auto-fix
    let fix_config = FixConfig {
        llm_config: Arc::new(llm_config),
        max_iterations: cli.max_iterations,
        verbose: cli.verbose,
        parallel: !cli.no_parallel,
        parallelism: cli.parallelism,
    };

    let summary = auto_fix_project(&working_dir, &fix_config)?;

    // Print summary
    info!("\n=== Fix Summary ===");
    info!("Project: {}", summary.project_name);
    info!("Iterations: {}", summary.total_iterations);
    info!("Initial errors: {}", summary.initial_error_count);
    info!("Final errors: {}", summary.final_error_count);
    info!("Files modified: {}", summary.files_modified.len());
    info!("Duration: {}", (summary.end_time - summary.start_time).num_seconds());

    if summary.final_success {
        info!("✅ Build succeeded!");
    } else {
        info!("⚠️  Build still has {} errors after {} iterations",
              summary.final_error_count, summary.total_iterations);
    }

    info!("\nDetailed summary saved to: {}", working_dir.history_dir.join("summary.json").display());

    Ok(())
}

fn load_llm_config(config_path: Option<&Path>) -> Result<LLMConfig, Box<dyn std::error::Error>> {
    // Try to load from specified config file, or default locations
    let config_path = if let Some(path) = config_path {
        path.to_path_buf()
    } else {
        // Try current directory
        let cwd = std::env::current_dir()?.join("config.toml");
        if cwd.exists() {
            cwd
        } else {
            // Try user config directory
            let config_dir = directories::ProjectDirs::from("", "", "harvest")
                .ok_or("Could not determine config directory")?;
            config_dir.config_dir().join("config.toml")
        }
    };

    if !config_path.exists() {
        return Err(format!("Config file not found: {}", config_path.display()).into());
    }

    info!("Loading config from: {}", config_path.display());

    let config_content = std::fs::read_to_string(&config_path)?;
    let config: Config = toml::from_str(&config_content)?;

    // Try auto_fix config first, fall back to compilation_unit_to_rust_llm
    let llm_config = if let Some(tool_cfg) = config.tools.get("auto_fix") {
        LLMConfig::deserialize(tool_cfg)?
    } else if let Some(tool_cfg) = config.tools.get("compilation_unit_to_rust_llm") {
        info!("Using compilation_unit_to_rust_llm config for auto_fix");
        LLMConfig::deserialize(tool_cfg)?
    } else if let Some(tool_cfg) = config.tools.get("raw_source_to_cargo_llm") {
        info!("Using raw_source_to_cargo_llm config for auto_fix");
        LLMConfig::deserialize(tool_cfg)?
    } else {
        return Err("No LLM configuration found in config.toml. Please add [tools.auto_fix] section".into());
    };

    Ok(llm_config)
}
