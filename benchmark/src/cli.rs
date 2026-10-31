use crate::harness::feature_combo::FeatureCombos;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "harvest-benchmark")]
#[command(
    about = "Runs all benchmarks by translating C projects to Rust and validating them with test vectors"
)]
pub struct Args {
    /// Input directory containing subdirectories with benchmarks
    #[arg(
        help = "Path to the directory containing example subdirectories (each with test_case/ and test_vectors/)"
    )]
    pub input_dir: PathBuf,

    /// Output directory where the translated Rust projects will be written
    #[arg(help = "Path to the output directory for all translated Rust projects")]
    pub output_dir: PathBuf,

    /// Use modular translation rather than standard all-at-once translation.
    #[arg(long, conflicts_with = "agentic")]
    pub modular: bool,

    /// Use the agentic translation tool.
    #[arg(long, conflicts_with = "modular")]
    pub agentic: bool,

    /// Run the agentic verify-and-fix stage after translation (requires --agentic).
    #[arg(long, requires = "agentic")]
    pub agentic_verify: bool,

    /// Set a configuration value; format $NAME=$VALUE.
    #[arg(long, short)]
    pub config: Vec<String>,

    /// Timeout in seconds for running test cases
    #[arg(long, default_value = "10")]
    pub timeout: u64,

    /// Number of LLM-based repair passes to attempt after a failed build.
    #[arg(long, default_value = "2")]
    pub repair_passes: usize,

    /// Filter benchmarks by regex pattern on directory names (keeps matching directories).
    /// Examples: ".*_lib$" (only libraries)
    /// Cannot be used together with --exclude.
    #[arg(long, conflicts_with = "exclude")]
    pub filter: Option<String>,

    /// Exclude benchmarks by regex pattern on directory names (removes matching directories).
    /// Examples: ".*_lib$" (exclude libraries)
    /// Cannot be used together with --filter.
    #[arg(long, conflicts_with = "filter")]
    pub exclude: Option<String>,

    /// Feature combination mode for testing translated crates.
    ///
    /// - `default` (default): exercise only the C build's default feature selection.
    ///   Produces the same results as previous `benchmark` runs; no behavior change for
    ///   the existing TRACTOR corpus.
    /// - `all`: iterate the full Cartesian product of feature combinations declared in
    ///   the translated crate's `[features]` block. Errors if the product exceeds 1024
    ///   combinations; use `--feature-combos N` to cap instead.
    /// - `N` (positive integer): sample exactly N combinations drawn evenly from the
    ///   full Cartesian product (deterministic, evenly-spaced indices). If the full
    ///   product is smaller than N, all combinations are tested.
    #[arg(long, default_value = "default", value_name = "MODE")]
    pub feature_combos: FeatureCombos,
}
