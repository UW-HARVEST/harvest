//! Utilities for recombining translated Rust declarations into a Cargo package.

use build_project_spec::ProjectKind;
use full_source::CargoPackage;
use harvest_core::fs::RawDir;
use std::collections::BTreeSet;
use tracing::debug;

use crate::translation::RustDeclaration;
use crate::translation::TranslationResult;

fn prepend_dependency_imports(translations: &[RustDeclaration], rust_code: String) -> String {
    let imports = translations
        .iter()
        .flat_map(|decl| decl.dependencies.iter())
        .map(|dep| dep.trim())
        .filter(|dep| !dep.is_empty())
        .map(|dep| format!("use {dep};"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n");

    (imports + "\n\n" + &rust_code).trim().into()
}

/// Recombines translated Rust declarations into a CargoPackage representation.
///
/// This function takes a translation result (declarations and Cargo.toml) and assembles them into
/// a complete Cargo project structure with:
/// - A Cargo.toml manifest (from the LLM translation response)
/// - A src/main.rs (for executables) or src/lib.rs (for libraries)
/// - All necessary imports derived from declaration dependencies
pub fn recombine_decls(
    translation_result: TranslationResult,
    project_kind: &ProjectKind,
) -> Result<CargoPackage, Box<dyn std::error::Error>> {
    debug!(
        "Recombining {} declarations",
        translation_result.translations.len()
    );

    // Concatenate translated macros first, then translated declarations.
    let macro_code = translation_result.macros.join("\n\n");
    let decl_code = translation_result
        .translations
        .iter()
        .map(|decl| decl.rust_code.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let rust_code = if macro_code.is_empty() {
        decl_code
    } else if decl_code.is_empty() {
        macro_code
    } else {
        format!("{macro_code}\n\n{decl_code}")
    };

    let source_content = prepend_dependency_imports(&translation_result.translations, rust_code);

    // Determine the main source file name based on project kind
    let source_file = match project_kind {
        ProjectKind::Executable => "src/main.rs",
        ProjectKind::Library => "src/lib.rs",
    };

    // Create the directory structure
    let mut dir = RawDir::default();
    dir.set_file("Cargo.toml", translation_result.cargo_toml.into_bytes())?;
    dir.set_file(source_file, source_content.into_bytes())?;

    Ok(CargoPackage { dir })
}
