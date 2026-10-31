//! Copies a built Cargo package from its temporary build directory to the configured output path.

use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;
use try_cargo_build::CargoBuildResult;

/// Copies a built Cargo package to a destination directory.
///
/// By default (`WriteOutput` / `WriteOutput::new(None)`) the destination is
/// `config.output`. An explicit override lets the pipeline emit an intermediate
/// stage (e.g. the pre-verify translation) to a separate folder so both the
/// translate-only and verified crates are preserved as independent outputs.
#[derive(Default)]
pub struct WriteOutput {
    dest_override: Option<PathBuf>,
}

impl WriteOutput {
    /// Write to an explicit directory instead of `config.output`.
    pub fn to(dest: PathBuf) -> Self {
        WriteOutput {
            dest_override: Some(dest),
        }
    }
}

impl Tool for WriteOutput {
    fn name(&self) -> &'static str {
        "write_output"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let build_result = context
            .ir_snapshot
            .get::<CargoBuildResult>(inputs[0])
            .ok_or("WriteOutput: no CargoBuildResult found in IR")?;

        let src = build_result.root_path();
        let dst = self
            .dest_override
            .as_ref()
            .unwrap_or(&context.config.output);
        copy_dir_all(src, dst)?;
        info!("Output written to {}", dst.display());

        let executable = build_result
            .artifacts
            .iter()
            .find_map(|a| a.executable.as_ref())
            .and_then(|e| e.as_std_path().strip_prefix(src).ok())
            .map(|rel| dst.join(rel));

        Ok(Box::new(WriteOutputResult {
            path: dst.clone(),
            executable,
        }))
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}

pub struct WriteOutputResult {
    pub path: PathBuf,
    pub executable: Option<PathBuf>,
}

impl std::fmt::Display for WriteOutputResult {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Output written to: {}", self.path.display())
    }
}

impl Representation for WriteOutputResult {
    fn name(&self) -> &'static str {
        "write_output_result"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}
