//! JSON-based error classifier: parses `cargo build --message-format=json` output
//! using `cargo_metadata` and groups diagnostics by file.

use crate::compiler::BuildOutput;
use cargo_metadata::diagnostic::DiagnosticLevel;
use serde::Serialize;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DiagLevel {
    Error,
    Warning,
}

/// A single rustc diagnostic (error or warning), including its full rendered text.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub level: DiagLevel,
    /// Error code, e.g. `"E0308"`. `None` if the diagnostic has no code.
    pub code: Option<String>,
    /// Short message, e.g. `"mismatched types"`.
    pub message: String,
    /// Source file that owns this diagnostic, e.g. `"src/lib.rs"`.
    pub file: String,
    /// 1-indexed line of the primary span.
    pub line: u32,
    /// 1-indexed column of the primary span.
    pub col: u32,
    /// Complete rendered diagnostic block, ready to paste into an LLM prompt.
    pub full_text: String,
}

/// Per-file summary: only files that have at least one error-level diagnostic.
/// Warnings are included alongside errors so the fixer has full context.
#[derive(Debug, Clone, Serialize)]
pub struct FileErrorReport {
    pub file_path: String,
    pub error_count: usize,
    pub warning_count: usize,
    /// All error-level diagnostics for this file (for line→declaration mapping).
    pub diagnostics: Vec<Diagnostic>,
    /// Formatted text of errors (and warnings) for this file, ready for the LLM.
    pub errors_text: String,
}

/// Top-level result of `classify_errors`.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorClassification {
    /// Only files that have >= 1 error-level diagnostic.
    pub files: Vec<FileErrorReport>,
    pub total_errors: usize,
    pub total_warnings: usize,
}

/// Parse `cargo build --message-format=json` output and group diagnostics by file.
/// Files with only warnings are excluded from the result.
pub fn classify_errors(build_output: &BuildOutput) -> ErrorClassification {
    debug!(
        "Parsing build JSON output ({} bytes)",
        build_output.stdout_bytes.len()
    );

    // per_file: file_path -> (errors, warnings)
    let mut per_file: HashMap<String, (Vec<Diagnostic>, Vec<Diagnostic>)> = HashMap::new();

    for message in cargo_metadata::Message::parse_stream(build_output.stdout_bytes.as_slice()) {
        let Ok(message) = message else { continue };
        let cargo_metadata::Message::CompilerMessage(comp_msg) = message else {
            continue;
        };

        let diag = &comp_msg.message;

        let level = match diag.level {
            DiagnosticLevel::Error | DiagnosticLevel::Ice => DiagLevel::Error,
            DiagnosticLevel::Warning => DiagLevel::Warning,
            // Skip notes, help, failure-notes, and unknown.
            _ => continue,
        };

        // Find the primary span to get a file location.
        let Some(primary_span) = diag.spans.iter().find(|s| s.is_primary) else {
            continue;
        };

        let full_text = diag
            .rendered
            .clone()
            .unwrap_or_else(|| diag.message.clone());

        let d = Diagnostic {
            level: level.clone(),
            code: diag.code.as_ref().map(|c| c.code.clone()),
            message: diag.message.clone(),
            file: primary_span.file_name.clone(),
            line: primary_span.line_start as u32,
            col: primary_span.column_start as u32,
            full_text,
        };

        let entry = per_file.entry(primary_span.file_name.clone()).or_default();
        match level {
            DiagLevel::Error => entry.0.push(d),
            DiagLevel::Warning => entry.1.push(d),
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

    // Build FileErrorReport for each file that has >= 1 error.
    let mut files: Vec<FileErrorReport> = per_file
        .into_iter()
        .filter(|(_, (errors, _))| !errors.is_empty())
        .map(|(file_path, (errors, warnings))| {
            let errors_text = build_errors_text(&file_path, &errors, &warnings);
            FileErrorReport {
                error_count: errors.len(),
                warning_count: warnings.len(),
                diagnostics: errors,
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
