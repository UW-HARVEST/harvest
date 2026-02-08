//! Utilities for working with Cargo manifests and projects.
//!
//! This module provides a collection of utilities for manipulating Cargo manifest files
//! (Cargo.toml) and managing Rust project structures. It uses the `toml_edit` crate to
//! preserve formatting and comments when modifying TOML documents.

use crate::error::HarvestResult;
use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, Value};

/// Extracts the package name from a Cargo.toml file.
///
/// # Arguments
/// * `manifest` - Path to the Cargo.toml file
///
/// # Returns
/// The package name if found, or `None` if parsing fails or the field doesn't exist.
///
/// # Example
/// ```toml
/// [package]
/// name = "my-library"
/// ```
/// Returns: `Some("my-library")`
pub fn read_package_name(manifest: &Path) -> Option<String> {
    let contents = fs::read_to_string(manifest).ok()?;
    let doc = contents.parse::<DocumentMut>().ok()?;

    doc.get("package")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}

/// Ensures a Cargo.toml has `"cdylib"` in its `[lib]` section's `crate-type` array.
///
/// This is required for library projects to generate dynamically loadable shared libraries
/// that can be called via FFI from test runners.
///
/// If the manifest already has `"cdylib"` in the `crate-type` array, this function does
/// nothing. If `crate-type` exists but doesn't contain `"cdylib"`, it adds `"cdylib"` to
/// the existing array while preserving other crate types. If no `crate-type` exists, it
/// creates `crate-type = ["cdylib"]`.
///
/// # Examples
/// - `crate-type = ["rlib"]` -> `crate-type = ["rlib", "cdylib"]`
/// - `crate-type = ["cdylib"]` -> no change
/// - `crate-type = "rlib"` -> `crate-type = ["rlib", "cdylib"]`
/// - No crate-type -> creates `crate-type = ["cdylib"]`
///
/// # Arguments
/// * `manifest` - Path to the Cargo.toml file to modify
///
/// # Errors
/// Returns an error if the manifest doesn't exist or cannot be read/written.
pub fn ensure_cdylib(manifest: &Path) -> HarvestResult<()> {
    if !manifest.exists() {
        return Err(format!("Cargo.toml not found at {}", manifest.display()).into());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    // Get or create [lib] section
    let lib = doc
        .entry("lib")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("Failed to access [lib] section")?;

    // Check if crate-type exists
    if let Some(crate_type_item) = lib.get("crate-type") {
        // crate-type exists, check if it's an array
        if let Some(array) = crate_type_item.as_array() {
            // Check if "cdylib" is already in the array
            let has_cdylib = array
                .iter()
                .any(|v| v.as_str().map(|s| s == "cdylib").unwrap_or(false));

            if has_cdylib {
                // Already has cdylib, nothing to do
                return Ok(());
            }

            // Need to add cdylib to existing array
            let mut new_array = array.clone();
            new_array.push("cdylib");
            lib.insert("crate-type", Item::Value(Value::Array(new_array)));
        } else if let Some(s) = crate_type_item.as_str() {
            // crate-type is a single string (rare case)
            if s == "cdylib" {
                return Ok(());
            }
            // Convert to array with both values
            let mut array = toml_edit::Array::new();
            array.push(s);
            array.push("cdylib");
            lib.insert("crate-type", Item::Value(Value::Array(array)));
        } else {
            // Unexpected format, replace with ["cdylib"]
            let mut array = toml_edit::Array::new();
            array.push("cdylib");
            lib.insert("crate-type", Item::Value(Value::Array(array)));
        }
    } else {
        // No crate-type, add ["cdylib"]
        let mut array = toml_edit::Array::new();
        array.push("cdylib");
        lib.insert("crate-type", Item::Value(Value::Array(array)));
    }

    // If we reach here, we must have modified the document
    fs::write(manifest, doc.to_string())?;
    Ok(())
}

/// Recursively copies a directory tree from source to destination.
///
/// This function silently succeeds if the source directory doesn't exist, making it
/// useful for copying optional directories.
///
/// # Arguments
/// * `src` - Source directory path
/// * `dst` - Destination directory path
///
/// # Errors
/// Returns an error if I/O operations fail during the copy process.
pub fn copy_directory_recursive(src: &Path, dst: &Path) -> HarvestResult<()> {
    if !src.exists() {
        return Ok(());
    }
    fn recurse(src: &Path, dst: &Path) -> HarvestResult<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                recurse(&path, &target)?;
            } else {
                fs::copy(&path, &target)?;
            }
        }
        Ok(())
    }
    recurse(src, dst)
}

/// Adds a `[workspace]` section to a Cargo.toml to prevent parent workspace interference.
///
/// When cargo builds a crate, it searches up the directory tree for workspace roots.
/// If it finds one, it treats the crate as a workspace member, which can break
/// path dependencies. Adding an empty `[workspace]` section declares the crate
/// as its own workspace root, stopping the upward search.
///
/// # Arguments
/// * `manifest` - Path to the Cargo.toml to modify
///
/// # Errors
/// Returns an error if the manifest cannot be read or written.
pub fn add_workspace_guard(manifest: &Path) -> HarvestResult<()> {
    if !manifest.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    // Check if [workspace] section already exists
    if doc.contains_key("workspace") {
        return Ok(());
    }

    // Add empty [workspace] section
    doc.insert("workspace", Item::Table(Table::new()));

    fs::write(manifest, doc.to_string())?;
    Ok(())
}

/// Updates a dependency's path in a Cargo.toml file.
///
/// This function updates the path for a dependency in the [dependencies] section.
/// The dependency must be specified as an inline table with a path field.
///
/// # Arguments
/// * `manifest` - Path to the Cargo.toml to modify
/// * `dep_name` - Name of the dependency to update (e.g., "cando2")
/// * `new_path` - New relative path for the dependency
///
/// # Errors
/// Returns an error if the manifest cannot be read or written, or if TOML parsing fails.
///
/// # Example
/// Updates: `cando2 = { path = "../../../../tools/cando2" }`
/// To:      `cando2 = { path = "../translated_tools/cando2" }`
pub fn update_dependency_path(
    manifest: &Path,
    dep_name: &str,
    new_path: &str,
) -> HarvestResult<()> {
    if !manifest.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    // Get [dependencies] section
    let Some(deps) = doc.get_mut("dependencies").and_then(|d| d.as_table_mut()) else {
        return Ok(()); // No dependencies section
    };

    // Find the dependency
    let Some(dep) = deps.get_mut(dep_name) else {
        return Ok(()); // Dependency not found
    };

    // Update inline table path
    if let Some(dep_table) = dep.as_inline_table_mut() {
        if dep_table.contains_key("path") {
            dep_table.insert("path", Value::from(new_path));
            fs::write(manifest, doc.to_string())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Test reading package name from a valid Cargo.toml
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_read_package_name_valid() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "test-package"
version = "0.1.0"
"#
        )
        .unwrap();

        let name = read_package_name(temp_file.path());
        assert_eq!(name, Some("test-package".to_string()));
    }

    /// Test reading from non-existent file
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_read_package_name_missing_file() {
        let name = read_package_name(Path::new("/nonexistent/Cargo.toml"));
        assert_eq!(name, None);
    }

    /// Test reading from Cargo.toml without package section
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_read_package_name_no_package() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#"[workspace]"#).unwrap();

        let name = read_package_name(temp_file.path());
        assert_eq!(name, None);
    }

    /// Test ensure_cdylib adds cdylib when missing
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_ensure_cdylib_adds_when_missing() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "mylib"

[lib]
crate-type = ["rlib"]
"#
        )
        .unwrap();

        ensure_cdylib(temp_file.path()).unwrap();

        let contents = fs::read_to_string(temp_file.path()).unwrap();
        assert!(contents.contains("cdylib"));
    }

    /// Test ensure_cdylib doesn't duplicate cdylib
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_ensure_cdylib_no_duplicate() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "mylib"

[lib]
crate-type = ["cdylib"]
"#
        )
        .unwrap();

        ensure_cdylib(temp_file.path()).unwrap();

        let contents = fs::read_to_string(temp_file.path()).unwrap();
        // Count occurrences of "cdylib" - should be 1
        let count = contents.matches("cdylib").count();
        assert_eq!(count, 1);
    }

    /// Test ensure_cdylib creates lib section when missing
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_ensure_cdylib_creates_lib_section() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "mylib"
"#
        )
        .unwrap();

        ensure_cdylib(temp_file.path()).unwrap();

        let contents = fs::read_to_string(temp_file.path()).unwrap();
        assert!(contents.contains("[lib]"));
        assert!(contents.contains("cdylib"));
    }

    /// Test add_workspace_guard adds workspace section
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_add_workspace_guard() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "test"
"#
        )
        .unwrap();

        add_workspace_guard(temp_file.path()).unwrap();

        let contents = fs::read_to_string(temp_file.path()).unwrap();
        assert!(contents.contains("[workspace]"));
    }

    /// Test add_workspace_guard doesn't duplicate workspace section
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_add_workspace_guard_no_duplicate() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "test"

[workspace]
"#
        )
        .unwrap();

        add_workspace_guard(temp_file.path()).unwrap();

        let contents = fs::read_to_string(temp_file.path()).unwrap();
        let count = contents.matches("[workspace]").count();
        assert_eq!(count, 1);
    }

    /// Test update_dependency_path updates path correctly
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_update_dependency_path() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "test"

[dependencies]
cando2 = {{ path = "../old/path" }}
"#
        )
        .unwrap();

        update_dependency_path(temp_file.path(), "cando2", "../new/path").unwrap();

        let contents = fs::read_to_string(temp_file.path()).unwrap();
        assert!(contents.contains("../new/path"));
        assert!(!contents.contains("../old/path"));
    }

    /// Test update_dependency_path handles missing dependency
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_update_dependency_path_missing_dep() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            r#"
[package]
name = "test"

[dependencies]
"#
        )
        .unwrap();

        // Should not error when dependency doesn't exist
        let result = update_dependency_path(temp_file.path(), "nonexistent", "../path");
        assert!(result.is_ok());
    }
}
