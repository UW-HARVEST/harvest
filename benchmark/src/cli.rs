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

    /// Which agent to use for agentic translation: kiro or claude (requires --agentic).
    #[arg(long, requires = "agentic")]
    pub agentic_agent: Option<String>,

    /// Claude model to use for agentic translation/verification (requires --agentic).
    /// Accepts short aliases ("sonnet", "opus", "haiku") or full model IDs.
    /// Defaults to "sonnet" when not specified.
    #[arg(long, requires = "agentic")]
    pub agentic_model: Option<String>,

    /// Provide the agent with pre-built analysis tools (c_sandbox, symbol_diff).
    /// Only meaningful when --agentic is set.
    #[arg(long, requires = "agentic")]
    pub agent_tools: bool,

    /// Set a configuration value; format $NAME=$VALUE.
    #[arg(long, short)]
    pub config: Vec<String>,

    /// Timeout in seconds for running test cases
    #[arg(long, default_value = "10")]
    pub timeout: u64,

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
}
