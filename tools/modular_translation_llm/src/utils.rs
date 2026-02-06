//! Utility functions for working with source locations and ranges.

use full_source::RawSource;

/// Extracts the file path from a SourceLocation.
pub fn get_file_from_location(loc: &Option<clang_ast::SourceLocation>) -> Option<String> {
    loc.as_ref()
        .and_then(|l| l.spelling_loc.as_ref())
        .map(|sl| sl.file.to_string())
}

/// Reads the text from a source file at the range specified by a SourceRange.
///
/// Looks up the file in the `RawSource` and extracts the bytes between the
/// begin and end locations in the range's `spelling_loc` fields
pub fn read_source_at_range(
    range: &clang_ast::SourceRange,
    raw_source: &RawSource,
) -> Result<String, Box<dyn std::error::Error>> {
    // Extract the spelling locations from begin and end
    let begin_loc = range
        .begin
        .spelling_loc
        .as_ref()
        .ok_or("No spelling_loc in begin SourceLocation")?;
    let end_loc = range
        .end
        .spelling_loc
        .as_ref()
        .ok_or("No spelling_loc in end SourceLocation")?;

    // Verify both locations are in the same file
    if begin_loc.file != end_loc.file {
        return Err(format!(
            "SourceRange spans multiple files: {} and {}",
            begin_loc.file, end_loc.file
        )
        .into());
    }

    // Get the file contents from RawSource
    let file_contents = raw_source.dir.get_file(begin_loc.file.as_ref())?;

    // Calculate the end position (end offset + token length to include the last token)
    let start = begin_loc.offset;
    let end = end_loc.offset + end_loc.tok_len;

    if end > file_contents.len() {
        return Err(format!(
            "Source range ({}..{}) exceeds file size ({})",
            start,
            end,
            file_contents.len()
        )
        .into());
    }

    let text_bytes = &file_contents[start..end];
    let text = std::str::from_utf8(text_bytes)?;

    Ok(text.to_string())
}
