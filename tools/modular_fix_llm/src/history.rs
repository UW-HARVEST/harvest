//! Writes per-iteration repair history to disk for inspection.

use crate::error_classifier::ErrorClassification;
use serde_json::to_string_pretty;
use std::fs;
use std::path::Path;

/// Save the source and (optionally) the error classification for one repair iteration.
///
/// Files are written to `<history_dir>/iter_<iteration>/`:
/// - `source.rs`   – assembled Rust source at the start of this iteration
/// - `errors.json` – serialised `ErrorClassification` (omitted when `errors` is `None`,
///   i.e. the build succeeded)
pub fn save_iteration(
    history_dir: &Path,
    iteration: usize,
    source: &str,
    errors: Option<&ErrorClassification>,
) -> Result<(), Box<dyn std::error::Error>> {
    let iter_dir = history_dir.join(format!("iter_{}", iteration));
    fs::create_dir_all(&iter_dir)?;

    fs::write(iter_dir.join("source.rs"), source)?;

    if let Some(classification) = errors {
        let json = to_string_pretty(classification)?;
        fs::write(iter_dir.join("errors.json"), json)?;
    }

    Ok(())
}
