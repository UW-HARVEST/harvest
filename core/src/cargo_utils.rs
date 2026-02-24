//! Utilities for working with Cargo manifests and projects.
//!
//! This module provides a collection of utilities for manipulating Cargo manifest files
//! (Cargo.toml) and managing Rust project structures. It uses the `toml_edit` crate to
//! preserve formatting and comments when modifying TOML documents.

use std::fs;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, Value};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Extracts the package name from a Cargo.toml file.
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

fn cdylib_array() -> toml_edit::Array {
    let mut array = toml_edit::Array::new();
    array.push("cdylib");
    array
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
/// # Errors
/// Returns an error if the manifest doesn't exist or cannot be read/written.
pub fn ensure_cdylib(manifest: &Path) -> Result<()> {
    if !manifest.exists() {
        return Err(format!("Cargo.toml not found at {}", manifest.display()).into());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    let lib = doc
        .entry("lib")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("Failed to access [lib] section")?;

    if let Some(crate_type_item) = lib.get("crate-type") {
        if let Some(array) = crate_type_item.as_array() {
            let has_cdylib = array
                .iter()
                .any(|v| v.as_str().map(|s| s == "cdylib").unwrap_or(false));

            if has_cdylib {
                return Ok(());
            }

            let mut new_array = array.clone();
            new_array.push("cdylib");
            lib.insert("crate-type", Item::Value(Value::Array(new_array)));
        } else if let Some(s) = crate_type_item.as_str() {
            if s == "cdylib" {
                return Ok(());
            }
            let mut array = toml_edit::Array::new();
            array.push(s);
            array.push("cdylib");
            lib.insert("crate-type", Item::Value(Value::Array(array)));
        } else {
            lib.insert("crate-type", Item::Value(Value::Array(cdylib_array())));
        }
    } else {
        lib.insert("crate-type", Item::Value(Value::Array(cdylib_array())));
    }

    fs::write(manifest, doc.to_string())?;
    Ok(())
}

/// Recursively copies a directory tree from source to destination.
///
/// This function silently succeeds if the source directory doesn't exist, making it
/// useful for copying optional directories.
///
/// # Errors
/// Returns an error if I/O operations fail during the copy process.
pub fn copy_directory_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    fn recurse(src: &Path, dst: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let target = dst.join(entry.file_name());
            if file_type.is_dir() {
                recurse(&entry.path(), &target)?;
            } else {
                fs::copy(entry.path(), &target)?;
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
/// # Errors
/// Returns an error if the manifest cannot be read or written.
pub fn add_workspace_guard(manifest: &Path) -> Result<()> {
    if !manifest.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    if doc.contains_key("workspace") {
        return Ok(());
    }

    doc.insert("workspace", Item::Table(Table::new()));

    fs::write(manifest, doc.to_string())?;
    Ok(())
}

/// Updates a dependency's path in a Cargo.toml file.
///
/// This function updates the path for a dependency in the [dependencies] section.
/// The dependency must be specified as an inline table with a path field.
///
/// # Errors
/// Returns an error if the manifest cannot be read or written, or if TOML parsing fails.
///
/// # Example
/// Updates: `cando2 = { path = "../../../../tools/cando2" }`
/// To:      `cando2 = { path = "../translated_tools/cando2" }`
pub fn update_dependency_path(manifest: &Path, dep_name: &str, new_path: &str) -> Result<()> {
    if !manifest.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    let Some(deps) = doc.get_mut("dependencies").and_then(|d| d.as_table_mut()) else {
        return Ok(());
    };

    let Some(dep) = deps.get_mut(dep_name) else {
        return Ok(());
    };

    if let Some(dep_table) = dep.as_inline_table_mut()
        && dep_table.contains_key("path")
    {
        dep_table.insert("path", Value::from(new_path));
        fs::write(manifest, doc.to_string())?;
    } else if let Some(dep_table) = dep.as_table_mut()
        && dep_table.contains_key("path")
    {
        dep_table.insert("path", Item::Value(Value::from(new_path)));
        fs::write(manifest, doc.to_string())?;
    }

    Ok(())
}

/// Force the package name to match the output directory name (sanitized). This keeps the
/// produced `lib<name>.so` aligned with the test runner's expected library stem and avoids
/// Cargo name errors.
///
/// Updates both `[package].name` and `[lib].name` (if present) to ensure the library
/// artifact filename matches expectations.
///
/// # Errors
/// Returns an error if the manifest cannot be read or written, or if TOML parsing fails.
pub fn normalize_package_name(manifest: &Path, project_dir: &Path) -> Result<()> {
    if !manifest.exists() {
        return Ok(());
    }
    let desired_raw = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if desired_raw.is_empty() {
        return Ok(());
    }
    let desired = sanitize_package_name(&desired_raw);
    if desired.is_empty() {
        return Ok(());
    }

    let contents = fs::read_to_string(manifest)?;
    let mut doc = contents
        .parse::<DocumentMut>()
        .map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    let mut changed = false;

    #[allow(clippy::collapsible_if)]
    if let Some(package) = doc.get_mut("package").and_then(|p| p.as_table_mut()) {
        if let Some(current_name) = package.get("name").and_then(|n| n.as_str()) {
            if current_name != desired {
                package.insert("name", Item::Value(Value::from(&desired)));
                changed = true;
            }
        }
    }

    if let Some(lib) = doc.get_mut("lib").and_then(|l| l.as_table_mut()) {
        let needs_update = if let Some(current_lib_name) = lib.get("name").and_then(|n| n.as_str())
        {
            current_lib_name != desired
        } else {
            true
        };

        if needs_update {
            lib.insert("name", Item::Value(Value::from(&desired)));
            changed = true;
        }
    }

    if changed {
        fs::write(manifest, doc.to_string())?;
    }

    Ok(())
}

/// Sanitize a package name so Cargo accepts it:
/// - replace invalid chars with `_`
/// - if it starts with a digit or `-`, prefix with `_`
pub fn sanitize_package_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => c,
            _ => '_',
        })
        .collect();
    if s.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        s.insert(0, '_');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_read_package_name_missing_file() {
        let name = read_package_name(Path::new("/nonexistent/Cargo.toml"));
        assert_eq!(name, None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_read_package_name_no_package() {
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, r#"[workspace]"#).unwrap();

        let name = read_package_name(temp_file.path());
        assert_eq!(name, None);
    }

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
        let count = contents.matches("cdylib").count();
        assert_eq!(count, 1);
    }

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

        let result = update_dependency_path(temp_file.path(), "nonexistent", "../path");
        assert!(result.is_ok());
    }
}
