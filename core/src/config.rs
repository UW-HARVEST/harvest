use std::{collections::HashMap, path::PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// Configuration for this harvest-translate run. The sources of these configuration values (from
/// highest-precedence to lowest-precedence) are:
///
/// 1. Configurations passed using the `--config` command line flag.
/// 2. A user-specific configuration directory (e.g. `$HOME/.config/harvest/config.toml').
/// 3. Defaults specified in the code (using `#[serde(default)]`).
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Path to the directory containing the C code to translate.
    pub input: PathBuf,

    /// Path to output directory.
    pub output: PathBuf,

    /// Path to the diagnostics directory, if you want diagnostics output. If you do not specify a
    /// diagnostics path, a temporary directory will be created (so that working directories can be
    /// created for tools) and cleaned up when translate completes.
    pub diagnostics_dir: Option<PathBuf>,

    /// For both the output directory and diagnostics directory (if enabled):
    /// If true: if the directory exists and is nonempty, translate will delete the contents of the
    /// directory before running.
    /// If false: if the directory exists and is nonempty, translate will output an error and exit.
    pub force: bool,

    /// If true, use modular translation (translating one declaration at a time).
    // If false, use standard all-at-once translation.
    pub modular: bool,

    /// If true (and `modular` is also true), run the iterative fix loop after translation
    /// to fix compilation errors before the final `TryCargoBuild` step.
    pub fix: bool,

    /// Maximum number of fix-loop iterations when `fix` is true.
    /// Each iteration runs `FixBuildCheck`, `DiagnosticAttributor`, and `FixDeclarationsLlm`.
    #[serde(default = "Config::default_max_fix_iterations")]
    pub max_fix_iterations: usize,

    /// Filter describing which log messages should be output to stdout. This is in the
    /// `tracing_subscriber::filter::EnvFilter` format.
    pub log_filter: String,

    /// Sub-configuration for each tool.
    pub tools: HashMap<String, serde_json::Value>,

    // serde will place any unrecognized fields here. This will be passed to unknown_field_warning
    // after parsing to emit warnings on unrecognized config entries (we don't error on unknown
    // fields because that can be annoying to work with if you are switching back and forth between
    // commits that have different config options).
    #[serde(flatten)]
    pub unknown: HashMap<String, serde_json::Value>,
}

impl Config {
    /// Returns a mock config for testing.
    pub fn mock() -> Self {
        Self {
            input: PathBuf::from("mock_input"),
            output: PathBuf::from("mock_output"),
            diagnostics_dir: None,
            force: false,
            modular: false,
            fix: false,
            max_fix_iterations: Self::default_max_fix_iterations(),
            log_filter: "off".to_owned(),
            tools: Default::default(),
            unknown: Default::default(),
        }
    }

    fn default_max_fix_iterations() -> usize {
        5
    }

    /// Returns formatted llm info.
    /// Printed at the start of translation and benchmarking runs to aid in reproduction of results.
    pub fn model_info(&self) -> Option<String> {
        let tool_name = if self.modular {
            "modular_translation_llm"
        } else {
            "raw_source_to_cargo_llm"
        };

        self.tools.get(tool_name).map(|tool| {
            let backend = tool
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let model = tool
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let max_tokens = tool
                .get("max_tokens")
                .map_or("<unknown>".to_owned(), |v| v.to_string());

            format!(
                "Backend={} Model={} Max Tokens={}",
                backend, model, max_tokens
            )
        })
    }
}

/// Prints out a warning message for every field in `unknown`.
///
/// This is intended for use by config validation routines. `prefix` should be the path to this
/// entry (e.g. `tools::Config` should call this with a `prefix` of `tools`).
pub fn unknown_field_warning(prefix: &str, unknown: &HashMap<String, Value>) {
    let mut entries: Vec<_> = unknown.keys().collect();
    entries.sort_unstable();
    entries.into_iter().for_each(|name| match prefix {
        "" => eprintln!("Warning: unknown config key {name}"),
        p => eprintln!("Warning: unknown config key {p}.{name}"),
    });
}
