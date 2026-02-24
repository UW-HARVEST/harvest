//! Symbolic (local) error classifier: parses `cargo build` output without calling an LLM.

use crate::compiler::BuildResult;
use serde::Serialize;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DiagLevel {
    Error,
    Warning,
}

/// A single rustc diagnostic (error or warning), including its full original text.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub level: DiagLevel,
    /// Error code, e.g. `"E0308"`. `None` if the diagnostic has no code.
    pub code: Option<String>,
    /// Short message, e.g. `"mismatched types"`.
    pub message: String,
    /// Relative file path that owns this diagnostic, e.g. `"src/deflate.rs"`.
    /// Derived from the *first* `-->` pointer in the block.
    pub file: String,
    pub line: u32,
    pub col: u32,
    /// Complete block text (header + source excerpt + notes/help), ready to
    /// paste into an LLM prompt.
    pub full_text: String,
}

/// Per-file summary produced by the classifier, containing only files that
/// have at least one error.  Warnings are included alongside errors so the
/// fixer has full context.
#[derive(Debug, Clone, Serialize)]
pub struct FileErrorReport {
    pub file_path: String,
    pub error_count: usize,
    pub warning_count: usize,
    /// Formatted diagnostic text for this file only.  This is what gets
    /// handed to the fixer LLM instead of the full build output.
    pub errors_text: String,
}

/// Top-level result of `classify_errors`.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorClassification {
    /// Only files that have ≥ 1 error-level diagnostic.
    pub files: Vec<FileErrorReport>,
    pub total_errors: usize,
    pub total_warnings: usize,
}

/// Parse `cargo build` / `cargo check` output and group diagnostics by file.
/// Files with only warnings are not included in the result; files with errors
/// are included together with any warnings they also carry.
pub fn classify_errors(build_result: &BuildResult) -> ErrorClassification {
    debug!(
        "Parsing build output ({} bytes)",
        build_result.combined_output.len()
    );

    let diagnostics = parse_diagnostics(&build_result.combined_output);

    // Group by file.
    // per_file: file -> (errors, warnings)
    let mut per_file: HashMap<String, (Vec<Diagnostic>, Vec<Diagnostic>)> = HashMap::new();

    for diag in &diagnostics {
        let entry = per_file.entry(diag.file.clone()).or_default();
        match diag.level {
            DiagLevel::Error => entry.0.push(diag.clone()),
            DiagLevel::Warning => entry.1.push(diag.clone()),
        }
    }

    let total_errors: usize = per_file.values().map(|(e, _)| e.len()).sum();
    let total_warnings: usize = per_file.values().map(|(_, w)| w.len()).sum();

    debug!(
        "Parsed {} errors, {} warnings across {} files",
        total_errors,
        total_warnings,
        per_file.len()
    );

    // Build FileErrorReport for each file that has ≥1 error.
    let mut files: Vec<FileErrorReport> = per_file
        .into_iter()
        .filter(|(_, (errors, _))| !errors.is_empty())
        .map(|(file_path, (errors, warnings))| {
            let errors_text = build_errors_text(&file_path, &errors, &warnings);
            FileErrorReport {
                error_count: errors.len(),
                warning_count: warnings.len(),
                errors_text,
                file_path,
            }
        })
        .collect();

    // Stable order: more errors first, then alphabetical.
    files.sort_by(|a, b| {
        b.error_count
            .cmp(&a.error_count)
            .then(a.file_path.cmp(&b.file_path))
    });

    for f in &files {
        debug!(
            "  {} ({} errors, {} warnings)",
            f.file_path, f.error_count, f.warning_count
        );
    }

    ErrorClassification {
        files,
        total_errors,
        total_warnings,
    }
}

/// Lines matched by the summary-line filter should NOT start a new diagnostic
/// block even though they begin with "error:" or "warning:".
fn is_summary_line(line: &str) -> bool {
    // "error: could not compile `...`"
    if line.starts_with("error: could not compile") {
        return true;
    }
    // "warning: `zlib` (lib) generated 215 warnings"
    // "warning: `zlib` (bin "example") generated 1 warning"
    if line.starts_with("warning:") {
        let rest = &line["warning:".len()..];
        if rest.contains("generated") && (rest.contains("warning") || rest.contains("error")) {
            return true;
        }
    }
    // "Some errors have detailed explanations: E0063, E0070, ..."
    if line.starts_with("Some errors have detailed explanations:") {
        return true;
    }
    // "For more information about an error, try `rustc --explain ...`"
    if line.starts_with("For more information about an error") {
        return true;
    }
    false
}

/// Return `Some((level, code, message))` if `line` is the start of a new
/// diagnostic block, i.e. it matches `^(error|warning)(\[E\d+\])?: `.
fn parse_diag_header(line: &str) -> Option<(DiagLevel, Option<String>, String)> {
    // Must start with "error" or "warning" at column 0.
    let (level, rest) = if let Some(r) = line.strip_prefix("error") {
        (DiagLevel::Error, r)
    } else if let Some(r) = line.strip_prefix("warning") {
        (DiagLevel::Warning, r)
    } else {
        return None;
    };

    // Optional `[Exxxx]` code.
    let (code, rest) = if let Some(r) = rest.strip_prefix('[') {
        if let Some(end) = r.find(']') {
            let code_str = &r[..end];
            (Some(code_str.to_string()), &r[end + 1..])
        } else {
            return None; // malformed
        }
    } else {
        (None, rest)
    };

    // Must be followed by ": ".
    let message = rest.strip_prefix(": ")?.to_string();
    if message.is_empty() {
        return None;
    }

    Some((level, code, message))
}

/// Extract `(file, line, col)` from a rustc location line like:
///   `   --> src/deflate.rs:908:17`
fn parse_location_line(line: &str) -> Option<(String, u32, u32)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("--> ")?;
    // rest = "src/deflate.rs:908:17"
    let mut parts = rest.splitn(3, ':');
    let file = parts.next()?.to_string();
    let ln: u32 = parts.next()?.parse().ok()?;
    let col: u32 = parts.next()?.trim().parse().ok()?;
    // Reject paths that look like Windows absolute or a summary artefact.
    if file.is_empty() || file.contains('\n') {
        return None;
    }
    Some((file, ln, col))
}

/// Walk all lines in the build output and produce a flat list of diagnostics.
fn parse_diagnostics(output: &str) -> Vec<Diagnostic> {
    let mut result: Vec<Diagnostic> = Vec::new();

    // State for the block currently being accumulated.
    let mut current_header: Option<(DiagLevel, Option<String>, String)> = None;
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_location: Option<(String, u32, u32)> = None;

    let flush = |header: Option<(DiagLevel, Option<String>, String)>,
                 lines: &[String],
                 location: Option<(String, u32, u32)>,
                 result: &mut Vec<Diagnostic>| {
        let (level, code, message) = match header {
            Some(h) => h,
            None => return,
        };
        let (file, line, col) = match location {
            Some(l) => l,
            // No location found — skip this block (it's probably a bare
            // "error: aborting due to..." that slipped through).
            None => return,
        };
        let full_text = lines.join("\n");
        result.push(Diagnostic {
            level,
            code,
            message,
            file,
            line,
            col,
            full_text,
        });
    };

    for raw_line in output.lines() {
        if is_summary_line(raw_line) {
            // Flush whatever is in flight, then skip this line entirely.
            flush(
                current_header.take(),
                &current_lines,
                current_location.take(),
                &mut result,
            );
            current_lines.clear();
            continue;
        }

        if let Some(header) = parse_diag_header(raw_line) {
            // Start of a new diagnostic block — flush the previous one first.
            flush(
                current_header.take(),
                &current_lines,
                current_location.take(),
                &mut result,
            );
            current_lines.clear();
            current_header = Some(header);
            current_lines.push(raw_line.to_string());
            // Location will be set when we see the first --> line.
        } else {
            // Continuation of the current block (or leading noise before the
            // first block, which is also fine — it will be discarded because
            // current_header is None).
            if current_location.is_none() {
                if let Some(loc) = parse_location_line(raw_line) {
                    current_location = Some(loc);
                }
            }
            current_lines.push(raw_line.to_string());
        }
    }

    // Flush the final block.
    flush(
        current_header.take(),
        &current_lines,
        current_location.take(),
        &mut result,
    );

    result
}

/// Format the errors (and any accompanying warnings) for one file into a
/// human-readable string suitable for feeding directly to the fixer LLM.
fn build_errors_text(file_path: &str, errors: &[Diagnostic], warnings: &[Diagnostic]) -> String {
    let mut out = String::new();
    out.push_str(&format!("COMPILATION ERRORS FOR {}:\n\n", file_path));
    for d in errors {
        out.push_str(&d.full_text);
        out.push_str("\n\n");
    }
    if !warnings.is_empty() {
        out.push_str(&format!("WARNINGS FOR {}:\n\n", file_path));
        for d in warnings {
            out.push_str(&d.full_text);
            out.push_str("\n\n");
        }
    }
    out
}
