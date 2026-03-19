use clang::source::SourceRange;
use std::path::Path;

use crate::{SourcePoint, SourceSpan};

pub(crate) fn is_c_or_header(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext == "c" || ext == "h",
        None => false,
    }
}

/// Generate appropriate libClang parser arguments based on the file type.
pub(crate) fn language_args_for_file(path: &Path) -> [&'static str; 2] {
    if path.extension().and_then(|ext| ext.to_str()) == Some("h") {
        ["-x", "c-header"]
    } else {
        ["-x", "c"]
    }
}

/// Read source text from a libClang SourceRange.
/// Returns None if the range is invalid or spans multiple files.
pub(crate) fn range_to_span_and_text(
    range: Option<SourceRange<'_>>,
    rel_file: &Path,
    file_bytes: &[u8],
) -> Option<(SourceSpan, String)> {
    let range = range?;
    let start = range.get_start().get_file_location();
    let end = range.get_end().get_file_location();

    let start_file = start.file?;
    let end_file = end.file?;
    // check if span is across multiple files
    let start_path = start_file.get_path();
    let end_path = end_file.get_path();
    if start_path != end_path {
        return None;
    }

    let start_offset = start.offset as usize;
    let end_offset = end.offset as usize;
    let source_text = String::from_utf8_lossy(&file_bytes[start_offset..end_offset]).to_string();

    let span = SourceSpan {
        file: rel_file.to_string_lossy().to_string(),
        start: SourcePoint {
            line: start.line,
            column: start.column,
            offset: start.offset,
        },
        end: SourcePoint {
            line: end.line,
            column: end.column,
            offset: end.offset,
        },
    };

    Some((span, source_text))
}
