//! A framework for translating C code into Rust code. This is normally used through the
//! `translate` binary, but is exposed as a library crate as well.

pub mod cli;
mod runner;
mod scheduler;
pub mod util;

use full_source::CargoPackage;
use harvest_core::config::Config;
use harvest_core::config::ProjectKindOverride;
use harvest_core::{HarvestIR, diagnostics};
use identify_project_kind::{IdentifyProjectKind, ProjectKind};
use load_raw_source::LoadRawSource;
use raw_source_to_cargo_llm::RawSourceToCargoLlm;
use runner::ToolRunner;
use scheduler::Scheduler;
use std::sync::Arc;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tracing::error;
use try_cargo_build::TryCargoBuild;
use walkdir::WalkDir;

struct FixedProjectKind(ProjectKind);

impl harvest_core::tools::Tool for FixedProjectKind {
    fn name(&self) -> &'static str {
        "fixed_project_kind"
    }

    fn run(
        self: Box<Self>,
        _context: harvest_core::tools::RunContext,
        _inputs: Vec<harvest_core::Id>,
    ) -> Result<Box<dyn harvest_core::Representation>, Box<dyn std::error::Error>> {
        Ok(Box::new(self.0))
    }
}

/// Performs the complete transpilation process using the scheduler.
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();
    // compile_commands mode: process each entry separately.
    if let Some(ref cc_path) = config.compile_commands {
        let entries = parse_compile_commands(cc_path)?;
        if entries.is_empty() {
            return Err("compile_commands.json is empty".into());
        }
        for entry in entries {
            let collector = diagnostics::Collector::initialize(&config)?;
            let mut local_cfg = (*config).clone();
            let (td, new_input, rel_rs) = synthesize_entry(&local_cfg, &entry)?;
            let _keep = td;
            local_cfg.input = new_input;
            if local_cfg.project_kind.is_none() {
                local_cfg.project_kind = Some(ProjectKindOverride::Executable);
            }
            {
                use serde_json::Value;
                let entry_cfg = local_cfg
                    .tools
                    .entry("raw_source_to_cargo_llm".into())
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(obj) = entry_cfg.as_object_mut() {
                    obj.insert("header_light".into(), Value::Bool(true));
                    obj.insert("single_out_path".into(), Value::String(rel_rs));
                }
            }
            let mut ir_local = HarvestIR::default();
            let mut runner = ToolRunner::new(collector.reporter());
            let mut scheduler = Scheduler::default();
            let load_src = scheduler.queue(LoadRawSource::new(&local_cfg.input));
            let identify_kind = if let Some(kind) = local_cfg.project_kind {
                let pk = match kind {
                    harvest_core::config::ProjectKindOverride::Executable => {
                        ProjectKind::Executable
                    }
                    harvest_core::config::ProjectKindOverride::Library => ProjectKind::Library,
                };
                scheduler.queue_after(FixedProjectKind(pk), &[load_src])
            } else {
                scheduler.queue_after(IdentifyProjectKind, &[load_src])
            };
            let _translate = scheduler.queue_after(RawSourceToCargoLlm, &[load_src, identify_kind]);
            let result = scheduler.run_all(&mut runner, &mut ir_local, Arc::new(local_cfg));
            drop(scheduler);
            drop(runner);
            if let Err(e) = result {
                error!("Error during transpilation: {e}");
                return Err(e);
            }
            if let Some((_, pkg)) = ir_local.get_by_representation::<CargoPackage>().next() {
                pkg.dir.materialize(&config.output)?;
            }
            collector.diagnostics();
        }
        return Ok(HarvestIR::default());
    }

    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let identify_kind = if let Some(kind) = config.project_kind {
        let pk = match kind {
            harvest_core::config::ProjectKindOverride::Executable => ProjectKind::Executable,
            harvest_core::config::ProjectKindOverride::Library => ProjectKind::Library,
        };
        scheduler.queue_after(FixedProjectKind(pk), &[load_src])
    } else {
        scheduler.queue_after(IdentifyProjectKind, &[load_src])
    };
    let translate = scheduler.queue_after(RawSourceToCargoLlm, &[load_src, identify_kind]);
    let _try_build = scheduler.queue_after(TryCargoBuild, &[translate]);

    // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
    let result = scheduler.run_all(&mut runner, &mut ir, config.clone().into());
    drop(scheduler);
    drop(runner);
    collector.diagnostics(); // TODO: Return this value (see issue 51)
    if let Err(e) = result {
        error!("Error during transpilation: {e}");
        return Err(e);
    }
    Ok(ir)
}
#[derive(serde::Deserialize)]
struct CcEntry {
    file: String,
    directory: String,
}

fn parse_compile_commands(path: &Path) -> Result<Vec<CcEntry>, Box<dyn std::error::Error>> {
    let entries: Vec<CcEntry> = serde_json::from_str(&fs::read_to_string(path)?)?;
    Ok(entries)
}

fn synthesize_entry(
    config: &Config,
    entry: &CcEntry,
) -> Result<(TempDir, PathBuf, String), Box<dyn std::error::Error>> {
    let cc_dir = config
        .compile_commands
        .as_ref()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .ok_or("compile_commands path missing")?;
    let project_root = config.project_root.as_ref().cloned().unwrap_or(cc_dir);

    let abs_c = {
        let p = PathBuf::from(&entry.file);
        if p.is_absolute() {
            p
        } else {
            PathBuf::from(&entry.directory).join(p)
        }
    };

    let tempdir = tempfile::tempdir()?;
    let out_root = tempdir.path().to_path_buf();

    let rel_c = abs_c
        .strip_prefix(&project_root)
        .unwrap_or_else(|_| Path::new(abs_c.file_name().unwrap_or_default()));
    let dest_c = out_root.join(rel_c);
    if let Some(parent) = dest_c.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&abs_c, &dest_c)?;

    // Parse includes to know which headers to embed.
    let mut include_names: HashSet<String> = HashSet::new();
    if let Ok(src) = fs::read_to_string(&abs_c) {
        for line in src.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("#include \"") {
                if let Some(end) = rest.find('"') {
                    include_names.insert(rest[..end].to_string());
                }
            }
        }
    }

    // Copy matching headers from project_root into tempdir.
    for entry in WalkDir::new(&project_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "h" && ext != "hpp" {
            continue;
        }
        if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
            if !include_names.is_empty() && !include_names.contains(fname) {
                continue;
            }
        }
        let rel = path
            .strip_prefix(&project_root)
            .unwrap_or_else(|_| Path::new(path.file_name().unwrap()));
        let dest = out_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::copy(path, dest);
    }

    let mut rel_rs = rel_c.to_path_buf();
    rel_rs.set_extension("rs");
    Ok((tempdir, out_root, rel_rs.to_string_lossy().to_string()))
}
