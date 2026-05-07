use cargo_metadata::diagnostic::Diagnostic;
use full_source::CargoPackage;
use quantize_rust_spans::RustItemMap;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::path::PathBuf;
use tracing::{info, warn};

pub(crate) type PatchSet = BTreeMap<(usize, usize), String>;

/// Apply a set of non-overlapping byte-range patches to `source` in ascending order.
///
/// `patches` maps `(start, end)` byte ranges (in the original, pre-patch coordinates) to
/// replacement strings. An `offset` is accumulated after each splice to translate original
/// coordinates into current coordinates as the source grows or shrinks.
pub(crate) fn apply_patches(source: &mut Vec<u8>, patches: PatchSet) {
    let mut offset: isize = 0;
    for ((orig_start, orig_end), new_entry) in patches {
        let diff = new_entry.len().cast_signed() - (orig_end - orig_start).cast_signed();
        source.splice(
            (orig_start.strict_add_signed(offset))..(orig_end.strict_add_signed(offset)),
            new_entry.bytes(),
        );
        offset += diff;
    }
}

pub(crate) fn generate_patches(
    decl_errors: &HashMap<(PathBuf, Bound<usize>, Bound<usize>), Vec<Diagnostic>>,
    cargo_package: &CargoPackage,
    fix_llm: &crate::fix_llm::FixLlm,
    interface_ctx: &str,
) -> Result<(HashMap<PathBuf, PatchSet>, usize), Box<dyn std::error::Error>> {
    let mut fixed_count = 0usize;
    let mut fixes: HashMap<PathBuf, PatchSet> = HashMap::new();

    for ((file_name, start, end), diagnostics) in decl_errors {
        let source = cargo_package.dir.get_file(file_name)?;
        let decl_source = str::from_utf8(&source[(*start, *end)])?;
        let errors_text = diagnostics
            .iter()
            .map(|d| d.message.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        info!(
            "FixDeclarationsLlm: LLM input declaration:\n{}",
            decl_source
        );
        match fix_llm.fix_declaration(decl_source, &errors_text, interface_ctx) {
            Ok(fixed) if !fixed.is_empty() => {
                info!("FixDeclarationsLlm: LLM output:\n{}", fixed);
                let start = match start {
                    Bound::Included(i) => *i,
                    Bound::Excluded(i) => i + 1,
                    Bound::Unbounded => 0,
                };
                let end = match end {
                    Bound::Included(i) => i + 1,
                    Bound::Excluded(i) => *i,
                    Bound::Unbounded => source.len(),
                };
                fixes
                    .entry(file_name.clone())
                    .or_default()
                    .insert((start, end), fixed);
                fixed_count += 1;
            }
            Ok(_) => {
                warn!("FixDeclarationsLlm: LLM returned empty response for declaration",);
            }
            Err(e) => {
                warn!("FixDeclarationsLlm: LLM fix failed for declaration: {}", e);
            }
        }
    }

    Ok((fixes, fixed_count))
}
