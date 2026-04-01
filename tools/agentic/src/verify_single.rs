//! Single-case verification via kiro-cli.
//!
//! After an initial translation, the verify stage asks the agent to review the Rust output against
//! the original C source and fix any issues it finds. The agent is given access to the full case
//! directory and can modify the translated Rust project in-place.

use std::path::Path;
use std::process::Command;
use tracing::{info, warn};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Default kiro-cli verification timeout in seconds (45 minutes).
const VERIFY_TIMEOUT_SECS: u64 = 2700;

/// Runs the verification agent on a single translated case.
///
/// # Directory layout
///
/// Expects the same layout produced by [`crate::translate_single::translate`]:
///
/// ```text
/// case_dir/
///   translated_rust/          <- Rust project to verify
///   translated_rust_original/ <- clean snapshot (restored before each verify run)
/// ```
///
/// The agent may modify files under `translated_rust/` in-place.
pub fn verify(case_dir: &Path, prompt_template: &str) -> Result<()> {
    let translated = case_dir.join("translated_rust");
    let original = case_dir.join("translated_rust_original");

    // Restore the clean snapshot so that verify always starts from a known state.
    if original.is_dir() {
        if translated.exists() {
            std::fs::remove_dir_all(&translated)?;
        }
        harvest_core::cargo_utils::copy_directory_recursive(&original, &translated)?;
    }

    let cmake_flags = extract_cmake_flags(case_dir);
    let prompt = prompt_template
        .replace("CASE_DIR_PLACEHOLDER", &case_dir.to_string_lossy())
        .replace("CMAKE_BUILD_FLAGS", &cmake_flags);

    let logs_dir = case_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join("verify.log");

    info!("Invoking kiro-cli for verification (timeout={}s)", VERIFY_TIMEOUT_SECS);

    let status = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "timeout {VERIFY_TIMEOUT_SECS} kiro-cli chat \
             --no-interactive --trust-all-tools \"$PROMPT\" < /dev/null 2>&1 | tee \"$LOG\"",
        ))
        .env("PROMPT", &prompt)
        .env("LOG", &log_path)
        .env(
            "OPENSSL_DIR",
            std::env::var("OPENSSL_DIR").unwrap_or_else(|_| "/usr".into()),
        )
        .current_dir(case_dir)
        .status()?;

    if !status.success() {
        warn!("kiro-cli verification exited with {status}");
    }

    info!("Verification complete");
    Ok(())
}

/// Extracts CMake cache variable flags from `CMakePresets.json`, if present.
///
/// These flags are injected into the verify prompt so the agent knows which build configuration
/// was active for this case.
fn extract_cmake_flags(case_dir: &Path) -> String {
    let presets = case_dir.join("translated_rust/c_src/CMakePresets.json");
    let content = match std::fs::read_to_string(&presets) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let data: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let Some(cv) = data
        .pointer("/configurePresets/1/cacheVariables")
        .and_then(|v| v.as_object())
    else {
        return String::new();
    };

    cv.iter()
        .filter(|(k, _)| *k != "CMAKE_C_STANDARD" && *k != "CMAKE_BUILD_TYPE")
        .map(|(k, v)| format!("-D{}={}", k, v.as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join(" ")
}
