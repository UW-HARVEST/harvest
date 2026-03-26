use std::path::Path;

use crate::{ClangAST, SourcePoint, SourceSpan, TopLevelEntity};

/// Checks if the file has a .c or .h extension, which indicates that we should parse it
pub(crate) fn is_c_or_header(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext == "c" || ext == "h",
        None => false,
    }
}

/// Returns true when a file path should be skipped by source parsers.
///
/// Currently excludes generated internals in any `CMakeFiles`, `.cache`, or
/// `build` directory.
pub(crate) fn should_skip_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| matches!(name, "CMakeFiles" | ".cache" | "build"))
    })
}

/// Generate appropriate libClang parser arguments based on the file type.
pub(crate) fn language_args_for_file(path: &Path) -> [&'static str; 2] {
    if path.extension().and_then(|ext| ext.to_str()) == Some("h") {
        ["-x", "c-header"]
    } else {
        ["-x", "c"]
    }
}

fn uses_spelling_location(child: &clang::Entity<'_>) -> bool {
    child.get_kind() == clang::EntityKind::MacroDefinition
}

/// Returns the entity location, resolved as spelling location for macros and
/// expansion location for all other declarations.
pub(crate) fn get_location<'tu>(
    child: &clang::Entity<'tu>,
) -> Option<clang::source::Location<'tu>> {
    let location = child.get_location()?;
    Some(if uses_spelling_location(child) {
        location.get_spelling_location()
    } else {
        location.get_expansion_location()
    })
}

/// Returns the underlying file for the entity location.
pub(crate) fn get_file_location<'tu>(
    child: &clang::Entity<'tu>,
) -> Option<clang::source::File<'tu>> {
    get_location(child)?.file
}

/// Returns start/end locations for an entity's range, resolved as spelling
/// locations for macros and expansion locations for all other declarations.
pub(crate) fn get_range<'tu>(
    child: &clang::Entity<'tu>,
) -> Option<(clang::source::Location<'tu>, clang::source::Location<'tu>)> {
    let range = child.get_range()?;
    Some(if uses_spelling_location(child) {
        (
            range.get_start().get_spelling_location(),
            range.get_end().get_spelling_location(),
        )
    } else {
        (
            range.get_start().get_expansion_location(),
            range.get_end().get_expansion_location(),
        )
    })
}

/// Read source text from a libClang entity location/range.
/// Returns None if the range is invalid or spans multiple files.
pub(crate) fn get_span_and_text(child: &clang::Entity<'_>) -> Option<(SourceSpan, String)> {
    let (start, end) = get_range(child)?;
    let start_file = get_file_location(child)?;
    let end_file = end.file?;
    // check if span is across multiple files
    let start_path = start_file.get_path();
    let end_path = end_file.get_path();
    if start_path != end_path {
        return None;
    }

    let file_bytes = std::fs::read(&start_path).ok()?;

    let start_offset = start.offset as usize;
    let end_offset = end.offset as usize;
    if start_offset > end_offset || end_offset > file_bytes.len() {
        return None;
    }
    let source_text = String::from_utf8_lossy(&file_bytes[start_offset..end_offset]).to_string();

    let span = SourceSpan {
        file: start_path.to_string_lossy().to_string(),
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

pub(crate) fn function_name(entity: &TopLevelEntity) -> Option<&str> {
    match entity.ast.as_ref() {
        Some(ClangAST::FunctionDecl { name, .. }) if !name.is_empty() => Some(name.as_str()),
        _ => None,
    }
}

pub(crate) fn is_header_file(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".h")
}

pub(crate) fn is_static_function(entity: &TopLevelEntity) -> bool {
    // Preferred source of truth when available.
    if let Some(ClangAST::FunctionDecl { storage_class, .. }) = entity.ast.as_ref()
        && let Some(storage_class) = storage_class
        && storage_class.eq_ignore_ascii_case("static")
    {
        return true;
    }
    false
}
