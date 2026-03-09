//! `FixBuildCheck`: materializes a `SplitPackage` to a temporary directory, runs
//! `cargo build --release --message-format=json`, and returns a `RawBuildResult`
//! containing structured diagnostics.

use cargo_metadata::diagnostic::DiagnosticLevel;
use harvest_core::cargo_utils::{add_workspace_guard, normalize_package_name};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use serde::Serialize;
use split_and_format::SplitPackage;
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tracing::debug;

/// Severity level of a rustc diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DiagLevel {
    Error,
    Warning,
}

/// Level of a child note or suggestion within a parent diagnostic.
#[derive(Debug, Clone, Serialize)]
pub enum RelatedLevel {
    Note,
    Help,
}

/// A child note or help span attached to a parent [`Diagnostic`].
///
/// The parent's `full_text` (from `rendered`) already contains this text;
/// `RelatedSpan` preserves the structured file/line information so that
/// `DiagnosticAttributor` can attribute context to any other declaration the note references.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedSpan {
    pub level: RelatedLevel,
    /// Short message, e.g. `"immutable borrow occurs here"`.
    pub message: String,
    /// Source file referenced by this note, e.g. `"src/lib.rs"`.
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// A single rustc diagnostic (error or warning) with its full rendered text.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub level: DiagLevel,
    /// Error code, e.g. `"E0308"`. `None` if the diagnostic has no code.
    pub code: Option<String>,
    /// Short message, e.g. `"mismatched types"`.
    pub message: String,
    /// Source file that owns this diagnostic, e.g. `"src/lib.rs"`.
    pub file: String,
    /// 1-indexed start line of the primary span.
    pub line_start: u32,
    /// 1-indexed end line of the primary span.
    pub line_end: u32,
    /// 1-indexed start column of the primary span.
    pub col_start: u32,
    /// 1-indexed end column of the primary span.
    pub col_end: u32,
    /// Complete rendered diagnostic block, ready to paste into an LLM prompt.
    /// Includes all child note/help text verbatim.
    pub full_text: String,
    /// Structured locations of child note/help spans.
    /// Used by `DiagnosticAttributor` to attribute context to other declarations.
    pub related_spans: Vec<RelatedSpan>,
}

/// Result of a build attempt on a `SplitPackage` in a temporary directory.
pub struct RawBuildResult {
    /// `true` if `cargo build` exited successfully (exit code 0).
    pub success: bool,
    /// All error- and warning-level diagnostics emitted by rustc.
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for RawBuildResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.success {
            writeln!(f, "RawBuildResult: success")?;
        } else {
            writeln!(
                f,
                "RawBuildResult: failed ({} diagnostics)",
                self.diagnostics.len()
            )?;
        }
        for d in &self.diagnostics {
            writeln!(
                f,
                "  [{:?}] {}:{} — {}",
                d.level, d.file, d.line_start, d.message
            )?;
        }
        Ok(())
    }
}

impl Representation for RawBuildResult {
    fn name(&self) -> &'static str {
        "raw_build_result"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        fs::write(path, self.to_string())
    }
}

// Helpers

struct BuildOutput {
    success: bool,
    stdout_bytes: Vec<u8>,
}

fn run_cargo_build(project_path: &Path) -> Result<BuildOutput, Box<dyn std::error::Error>> {
    add_workspace_guard(&project_path.join("Cargo.toml"))?;
    normalize_package_name(&project_path.join("Cargo.toml"), project_path)?;

    debug!(
        "FixBuildCheck: running cargo build in {}",
        project_path.display()
    );

    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--message-format=json")
        .current_dir(project_path)
        .output()
        .map_err(|e| {
            format!(
                "Failed to run cargo build in {}: {}",
                project_path.display(),
                e
            )
        })?;

    Ok(BuildOutput {
        success: output.status.success(),
        stdout_bytes: output.stdout,
    })
}

fn parse_diagnostics(build_output: &BuildOutput) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for message in cargo_metadata::Message::parse_stream(build_output.stdout_bytes.as_slice()) {
        let Ok(message) = message else { continue };
        let cargo_metadata::Message::CompilerMessage(comp_msg) = message else {
            continue;
        };

        let diag = &comp_msg.message;

        let level = match diag.level {
            DiagnosticLevel::Error | DiagnosticLevel::Ice => DiagLevel::Error,
            DiagnosticLevel::Warning => DiagLevel::Warning,
            // Skip top-level notes, helps, and failure-notes (e.g. "aborting due to N errors").
            // Their content is captured inside parent errors via `rendered`.
            _ => continue,
        };

        let Some(primary_span) = diag.spans.iter().find(|s| s.is_primary) else {
            continue;
        };

        let full_text = diag
            .rendered
            .clone()
            .unwrap_or_else(|| diag.message.clone());

        // Extract structured locations from child note/help diagnostics so that
        // DiagnosticAttributor can attribute context to the referenced declarations.
        let related_spans = diag
            .children
            .iter()
            .filter_map(|child| {
                let level = match child.level {
                    DiagnosticLevel::Note => RelatedLevel::Note,
                    DiagnosticLevel::Help => RelatedLevel::Help,
                    _ => return None,
                };
                // Prefer the primary span; fall back to the first available span.
                let span = child
                    .spans
                    .iter()
                    .find(|s| s.is_primary)
                    .or_else(|| child.spans.first())?;
                Some(RelatedSpan {
                    level,
                    message: child.message.clone(),
                    file: span.file_name.clone(),
                    line_start: span.line_start as u32,
                    line_end: span.line_end as u32,
                    col_start: span.column_start as u32,
                    col_end: span.column_end as u32,
                })
            })
            .collect();

        diagnostics.push(Diagnostic {
            level,
            code: diag.code.as_ref().map(|c| c.code.clone()),
            message: diag.message.clone(),
            file: primary_span.file_name.clone(),
            line_start: primary_span.line_start as u32,
            line_end: primary_span.line_end as u32,
            col_start: primary_span.column_start as u32,
            col_end: primary_span.column_end as u32,
            full_text,
            related_spans,
        });
    }

    diagnostics
}

// Tool

/// Builds a `SplitPackage` in a fresh temporary directory and returns structured diagnostics.
///
/// Uses a temporary directory so the fix-loop builds never pollute `config.output`.
/// The final verification build (writing to `config.output`) is handled by `TryCargoBuild`.
pub struct FixBuildCheck;

impl Tool for FixBuildCheck {
    fn name(&self) -> &'static str {
        "fix_build_check"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let split_pkg = context
            .ir_snapshot
            .get::<SplitPackage>(inputs[0])
            .ok_or("FixBuildCheck: no SplitPackage found in IR")?;

        let temp_dir = TempDir::new()?;
        split_pkg.materialize(temp_dir.path())?;

        let build_output = run_cargo_build(temp_dir.path())?;
        let diagnostics = parse_diagnostics(&build_output);

        debug!(
            "FixBuildCheck: success={}, diagnostics={}",
            build_output.success,
            diagnostics.len()
        );

        Ok(Box::new(RawBuildResult {
            success: build_output.success,
            diagnostics,
        }))
    }
}
