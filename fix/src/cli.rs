//! CLI argument parsing

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "fix")]
#[command(about = "Automatically fix Rust compilation errors using LLM", long_about = None)]
pub struct Cli {
    /// Input directory (must have Cargo.toml)
    #[arg(short, long, value_name = "DIR")]
    pub input: PathBuf,

    /// Output directory (where fixed project will be created)
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,

    /// Maximum number of fix iterations
    #[arg(long, default_value = "10")]
    pub max_iterations: usize,

    /// Configuration file path
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    /// Disable parallel file fixing (default: parallel enabled)
    #[arg(long)]
    pub no_parallel: bool,

    /// Maximum number of parallel threads (default: 10)
    #[arg(long, default_value = "10")]
    pub parallelism: usize,

    /// Only analyze errors, don't fix
    #[arg(long)]
    pub analyze_only: bool,
}
