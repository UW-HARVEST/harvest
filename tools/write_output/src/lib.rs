//! Copies a built Cargo package from its temporary build directory to the configured output path.

use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;
use try_cargo_build::CargoBuildResult;

pub struct WriteOutput;

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
        let dst = &context.config.output;
        copy_dir_all(src, dst)?;
        info!("Output written to {}", dst.display());

        Ok(Box::new(WriteOutputResult { path: dst.clone() }))
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
