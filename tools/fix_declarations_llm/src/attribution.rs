use cargo_metadata::diagnostic::{Diagnostic, DiagnosticLevel};
use full_source::CargoPackage;
use quantize_rust_spans::RustItemMap;
use std::collections::HashMap;
use std::ops::{Bound, RangeBounds as _};
use std::path::PathBuf;
use tracing::info;
use try_cargo_build::CargoBuildResult;

fn find_enclosing_decl(
    item_map: &RustItemMap,
    file_name: &PathBuf,
    byte_start: usize,
    byte_end: usize,
) -> Result<(Bound<usize>, Bound<usize>), Box<dyn std::error::Error>> {
    let items = item_map
        .items
        .get(file_name)
        .ok_or("FixDeclarationsLlm: no items found for file")?;

    for v in items {
        if v.contains(&byte_start) && v.contains(&byte_end) {
            return Ok((v.start_bound().cloned(), v.end_bound().cloned()));
        }
    }

    Ok((Bound::Unbounded, Bound::Unbounded))
}

fn get_error_diagnostics(
    build_result: &CargoBuildResult,
) -> Vec<&cargo_metadata::diagnostic::DiagnosticMessage> {
    let mut error_diagnostics = vec![];
    for d in &build_result.diagnostics {
        if d.message.level == DiagnosticLevel::Error {
            error_diagnostics.push(&d.message);
        }
    }

    info!(
        "FixDeclarationsLlm: {} error diagnostics found",
        error_diagnostics.len()
    );
    for msg in &error_diagnostics {
        info!("FixDeclarationsLlm: error diagnostic: {:#?}", msg);
    }

    error_diagnostics
}

pub(crate) fn attribute_errors(
    build_result: &CargoBuildResult,
    item_map: &RustItemMap,
    cargo_package: &CargoPackage,
) -> Result<
    HashMap<(PathBuf, Bound<usize>, Bound<usize>), Vec<Diagnostic>>,
    Box<dyn std::error::Error>,
> {
    let error_diagnostics = get_error_diagnostics(build_result);

    let mut decl_errors: HashMap<(PathBuf, Bound<usize>, Bound<usize>), Vec<Diagnostic>> =
        HashMap::new();

    for msg in error_diagnostics {
        for span in &msg.spans {
            let file_name = PathBuf::new().join(&span.file_name);
            cargo_package.dir.get_file(&file_name)?;

            let (decl_start, decl_end) = find_enclosing_decl(
                item_map,
                &file_name,
                span.byte_start as usize,
                span.byte_end as usize,
            )?;

            decl_errors
                .entry((file_name, decl_start, decl_end))
                .or_default()
                .push(msg.clone());
        }
    }

    Ok(decl_errors)
}
