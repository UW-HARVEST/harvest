use clang::source::SourceRange;
use std::{collections::HashMap, path::Path};

use crate::{SourcePoint, SourceSpan};

pub(crate) fn is_c_or_header(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.eq_ignore_ascii_case("c") || ext.eq_ignore_ascii_case("h"),
        None => false,
    }
}

pub(crate) fn language_args_for_file(path: &str) -> [&'static str; 2] {
    if path.ends_with(".h") {
        ["-x", "c-header"]
    } else {
        ["-x", "c"]
    }
}

pub(crate) fn normalize_rel_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

pub(crate) fn range_to_span_and_text(
    range: Option<SourceRange<'_>>,
    root_dir: &Path,
    file_bytes: &HashMap<String, Vec<u8>>,
) -> Option<(SourceSpan, String)> {
    let range = range?;
    let start = range.get_start().get_file_location();
    let end = range.get_end().get_file_location();

    let start_file = start.file?;
    let end_file = end.file?;
    let start_path = start_file.get_path();
    let end_path = end_file.get_path();
    if start_path != end_path {
        return None;
    }

    let rel_path = start_path
        .strip_prefix(root_dir)
        .ok()
        .map(normalize_rel_path)
        .unwrap_or_else(|| start_path.to_string_lossy().replace('\\', "/"));

    let bytes = file_bytes.get(&rel_path)?;
    let start_offset = start.offset as usize;
    let end_offset = end.offset as usize;
    if start_offset > end_offset || end_offset > bytes.len() {
        return None;
    }

    let source_text = String::from_utf8_lossy(&bytes[start_offset..end_offset]).to_string();

    let span = SourceSpan {
        file: rel_path,
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
