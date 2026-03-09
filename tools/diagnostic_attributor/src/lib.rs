//! `DiagnosticAttributor`: maps diagnostics from a `RawBuildResult` to the specific
//! declarations in a `SplitPackage`, using the pre-computed `line_index`.

use fix_build_check::{DiagLevel, Diagnostic, RawBuildResult};
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use split_and_format::SplitPackage;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use tracing::warn;

/// Diagnostics from a `RawBuildResult` grouped by declaration index.
pub struct DeclarationDiagnostics {
    /// `true` if at least one diagnostic is at the `Error` level.
    pub has_errors: bool,

    /// `decl_idx` -> list of error `full_text` strings.
    /// Only declarations that have at least one error appear here.
    pub decl_errors: HashMap<usize, Vec<String>>,

    /// `decl_idx` -> list of warning `full_text` strings.
    ///
    /// Contains two kinds of entries:
    /// - Direct warnings whose primary span falls inside the declaration.
    /// - Context entries: the `full_text` of error diagnostics whose child
    ///   notes/helps reference a line inside this declaration. These inform the
    ///   LLM that the declaration participates in an error located elsewhere.
    pub decl_warnings: HashMap<usize, Vec<String>>,
}

impl fmt::Display for DeclarationDiagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "DeclarationDiagnostics: has_errors={}, error_decls={}, warning_decls={}",
            self.has_errors,
            self.decl_errors.len(),
            self.decl_warnings.len()
        )
    }
}

impl Representation for DeclarationDiagnostics {
    fn name(&self) -> &'static str {
        "declaration_diagnostics"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

// Helpers

/// Look up which declaration index owns `line` (1-indexed) in `line_index`.
fn find_decl(line_index: &[(usize, usize, usize)], line: usize) -> Option<usize> {
    line_index
        .iter()
        .find(|&&(start, end, _)| line >= start && line <= end)
        .map(|&(_, _, idx)| idx)
}

/// Walk `diag.related_spans` and add `diag.full_text` as context into
/// `decl_warnings` for every declaration that a child note/help references,
/// skipping entries already present in `decl_errors` for that declaration.
fn add_related_context(
    diag: &Diagnostic,
    split_pkg: &SplitPackage,
    decl_errors: &HashMap<usize, Vec<String>>,
    decl_warnings: &mut HashMap<usize, Vec<String>>,
) {
    for span in &diag.related_spans {
        // Only attribute spans that belong to the source file we compiled.
        // This should always be true for ModularTranslationLLM-generated code.
        if span.file != split_pkg.source_file_name {
            continue;
        }

        let Some(idx) = find_decl(&split_pkg.line_index, span.line_start as usize) else {
            continue;
        };

        // Skip if this declaration already has the exact text as a primary error.
        if decl_errors
            .get(&idx)
            .is_some_and(|v| v.contains(&diag.full_text))
        {
            continue;
        }

        // Avoid duplicates: multiple children of the same error may point to the same declaration.
        let warnings = decl_warnings.entry(idx).or_default();
        if !warnings.contains(&diag.full_text) {
            warnings.push(diag.full_text.clone());
        }
    }
}

/// Maps each diagnostic in `RawBuildResult` to the declaration in `SplitPackage` that
/// owns the reported line, using `SplitPackage.line_index`.
///
/// Pass 1: primary attribution
/// errors -> `decl_errors`
/// warnings -> `decl_warnings`
/// keyed by the declaration that contains the primary span.
///
/// Pass 2: context attribution
/// For every error diagnostic, each child note/help span that (1) belongs to the same
/// compiled source file and (2) falls inside a different declaration gets the parent
/// error's `full_text` added to that declaration's `decl_warnings`. This puts the
/// referenced declaration on notice that it participates in an error even though the
/// primary span is elsewhere.
pub struct DiagnosticAttributor;

impl Tool for DiagnosticAttributor {
    fn name(&self) -> &'static str {
        "diagnostic_attributor"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let split_pkg = context
            .ir_snapshot
            .get::<SplitPackage>(inputs[0])
            .ok_or("DiagnosticAttributor: no SplitPackage found in IR")?;

        let build_result = context
            .ir_snapshot
            .get::<RawBuildResult>(inputs[1])
            .ok_or("DiagnosticAttributor: no RawBuildResult found in IR")?;

        let mut decl_errors: HashMap<usize, Vec<String>> = HashMap::new();
        let mut decl_warnings: HashMap<usize, Vec<String>> = HashMap::new();

        // Pass 1: primary span attribution
        for diag in &build_result.diagnostics {
            match find_decl(&split_pkg.line_index, diag.line_start as usize) {
                Some(idx) => match &diag.level {
                    DiagLevel::Error => {
                        decl_errors
                            .entry(idx)
                            .or_default()
                            .push(diag.full_text.clone());
                    }
                    DiagLevel::Warning => {
                        decl_warnings
                            .entry(idx)
                            .or_default()
                            .push(diag.full_text.clone());
                    }
                },
                None => {
                    warn!(
                        "DiagnosticAttributor: cannot map diagnostic on line {} to a declaration",
                        diag.line_start
                    );
                }
            }
        }

        // Pass 2: context attribution via related_spans
        for diag in &build_result.diagnostics {
            if diag.level != DiagLevel::Error {
                continue;
            }
            add_related_context(diag, split_pkg, &decl_errors, &mut decl_warnings);
        }

        let has_errors = !decl_errors.is_empty();

        Ok(Box::new(DeclarationDiagnostics {
            has_errors,
            decl_errors,
            decl_warnings,
        }))
    }
}
