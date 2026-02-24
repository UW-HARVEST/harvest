//! Version management and history tracking

use crate::compiler::BuildResult;
use std::fs;
use std::path::PathBuf;
use tracing::debug;

/// Working directory structure
#[derive(Debug, Clone)]
pub struct WorkingDirectory {
    pub root: PathBuf,
    pub history_dir: PathBuf,
    pub log_file: PathBuf,
    pub iteration_all_dir: PathBuf,
}

/// Save a complete snapshot of the iteration
pub fn save_iteration_snapshot(
    working_dir: &WorkingDirectory,
    iteration: usize,
    build_result: &BuildResult,
) -> Result<(), Box<dyn std::error::Error>> {
    debug!("Saving iteration {} snapshot", iteration);

    // Create iteration directory
    let iter_dir = working_dir
        .history_dir
        .join(format!("iteration_{}", iteration));
    fs::create_dir_all(&iter_dir)?;

    // Save build output
    let build_output_path = iter_dir.join("build_output.txt");
    fs::write(&build_output_path, &build_result.combined_output)?;

    // Save a complete copy of the entire project at this iteration
    let snapshot_dir = iter_dir.join("snapshot");
    let options = fs_extra::dir::CopyOptions::new();

    // Copy all files except .fix_history
    for entry in fs::read_dir(&working_dir.root)? {
        let entry = entry?;
        let path = entry.path();

        // Skip .fix_history directory itself
        if path.file_name().and_then(|n| n.to_str()) == Some(".fix_history") {
            continue;
        }

        let dest = snapshot_dir.join(path.file_name().unwrap());

        if path.is_dir() {
            fs::create_dir_all(&snapshot_dir)?;
            fs_extra::dir::copy(&path, &snapshot_dir, &options)?;
        } else {
            fs::create_dir_all(&snapshot_dir)?;
            fs::copy(&path, &dest)?;
        }
    }

    debug!("Saved complete snapshot to {}", snapshot_dir.display());

    Ok(())
}

/// Save a versioned copy of a file to iteration_all directory
/// Files are saved as: iteration_all/path/to/file.N.rs where N is the iteration number
pub fn save_file_version(
    working_dir: &WorkingDirectory,
    file_path: &str,
    iteration: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let source_path = working_dir.root.join(file_path);

    if !source_path.exists() {
        return Err(format!("Source file does not exist: {}", source_path.display()).into());
    }

    // Parse the file path to get directory and file name
    let file_path_buf = std::path::Path::new(file_path);
    let parent_dir = file_path_buf.parent();
    let file_name = file_path_buf
        .file_name()
        .ok_or("Invalid file path")?
        .to_str()
        .ok_or("Invalid file name")?;

    // Split extension
    let (base_name, ext) = if let Some(pos) = file_name.rfind('.') {
        (&file_name[..pos], &file_name[pos..])
    } else {
        (file_name, "")
    };

    // Create versioned filename: base.N.ext
    let versioned_name = format!("{}.{}{}", base_name, iteration, ext);

    // Create destination path preserving directory structure
    let dest_dir = if let Some(parent) = parent_dir {
        working_dir.iteration_all_dir.join(parent)
    } else {
        working_dir.iteration_all_dir.clone()
    };

    fs::create_dir_all(&dest_dir)?;
    let dest_path = dest_dir.join(versioned_name);

    // Copy the file
    fs::copy(&source_path, &dest_path)?;

    debug!(
        "Saved file version: {} -> {}",
        file_path,
        dest_path.display()
    );

    Ok(())
}

/// Save initial versions of all Rust source files (iteration 0)
pub fn save_initial_versions(
    working_dir: &WorkingDirectory,
) -> Result<(), Box<dyn std::error::Error>> {
    debug!("Saving initial versions of all source files");

    // Walk through the source directory
    fn walk_and_save(
        working_dir: &WorkingDirectory,
        current_dir: &std::path::Path,
        base_dir: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        for entry in fs::read_dir(current_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Skip .fix_history and target directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name == ".fix_history" || name == "target" {
                    continue;
                }
            }

            if path.is_dir() {
                walk_and_save(working_dir, &path, base_dir)?;
            } else if path.is_file() {
                // Only save .rs files
                if let Some(ext) = path.extension() {
                    if ext == "rs" {
                        // Get relative path from project root
                        let rel_path = path.strip_prefix(base_dir)?;
                        let rel_path_str = rel_path.to_str().ok_or("Invalid path")?;

                        // Save as version 0
                        save_file_version(working_dir, rel_path_str, 0)?;
                    }
                }
            }
        }
        Ok(())
    }

    let src_dir = working_dir.root.join("src");
    if src_dir.exists() {
        walk_and_save(working_dir, &src_dir, &working_dir.root)?;
    }

    debug!("Saved initial versions");
    Ok(())
}
