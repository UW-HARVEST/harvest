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

    /// Set a configuration value; format $NAME=$VALUE.
    #[arg(long, short)]
    pub config: Vec<String>,

    /// Timeout in seconds for running test cases
    #[arg(long, default_value = "10")]
    pub timeout: u64,

    /// Skip benchmarks whose directory names end with `_lib` (library runners)
    #[arg(long = "no-lib", default_value_t = false)]
    pub no_lib: bool,
}
