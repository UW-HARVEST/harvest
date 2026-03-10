use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};

use full_source::RawSource;
use harvest_core::Id;
use harvest_core::Representation;
use harvest_core::config::unknown_field_warning;
use harvest_core::fs::RawDir;
use harvest_core::llm::LLMConfig;
use harvest_core::tools::{RunContext, Tool};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info};

mod build_analyzer_llm;

pub use build_analyzer_llm::BuildAnalyzerLLM;

#[derive(Debug, Deserialize, Clone, Copy)]
pub enum ProjectKind {
    Library,
    Executable,
}

impl Display for ProjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectKind::Library => write!(f, "Library"),
            ProjectKind::Executable => write!(f, "Executable"),
        }
    }
}

pub struct ProjectSpec {
    pub targets: HashMap<PathBuf, TargetSpec>,
    pub target_order: Vec<PathBuf>,
}

pub struct TargetSpec {
    pub kind: ProjectKind,
    pub sources: RawSource,
}

pub struct ProjectTarget {
    pub artifact: PathBuf,
    pub kind: ProjectKind,
    pub sources: RawSource,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub llm: LLMConfig,

    #[serde(flatten)]
    unknown: HashMap<String, Value>,
}

impl Config {
    pub fn validate(&self) {
        unknown_field_warning("tools.build_project_spec", &self.unknown);
    }
}

impl Display for ProjectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProjectSpec(targets={})", self.targets.len())
    }
}

impl Representation for ProjectSpec {
    fn name(&self) -> &'static str {
        "project_spec"
    }
}

impl Display for ProjectTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProjectTarget(artifact={})", self.artifact.display())
    }
}

impl Representation for ProjectTarget {
    fn name(&self) -> &'static str {
        "project_target"
    }
}

pub struct BuildProjectSpec;
pub struct SelectPrimaryTarget;

fn clone_raw_source(raw_source: &RawSource) -> Result<RawSource, Box<dyn std::error::Error>> {
    let mut out = RawDir::default();
    for (path, contents) in raw_source.dir.files_recursive() {
        out.set_file(&path, contents.to_vec())
            .map_err(|e| format!("failed cloning file '{}' into RawDir: {e}", path.display()))?;
    }
    Ok(RawSource { dir: out })
}

fn collect_cmakelists_map(raw_source: &RawSource) -> HashMap<String, String> {
    raw_source
        .dir
        .files_recursive()
        .into_iter()
        .filter_map(|(path, contents)| {
            (path
                .file_name()
                .is_some_and(|name| name == "CMakeLists.txt"))
            .then_some((
                path.to_string_lossy().into_owned(),
                String::from_utf8_lossy(contents).into_owned(),
            ))
        })
        .collect()
}

fn has_allowed_source_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "c" | "h"))
        .unwrap_or(false)
}

fn source_tree_files(raw_source: &RawSource) -> HashSet<PathBuf> {
    raw_source
        .dir
        .files_recursive()
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

impl Tool for BuildProjectSpec {
    fn name(&self) -> &'static str {
        "build_project_spec"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = Config::deserialize(
            context
                .config
                .tools
                .get("build_project_spec")
                .ok_or("No build_project_spec config found")?,
        )?;
        config.validate();
        debug!("LLM Configuration {config:?}");

        // Get RawSource representation (the first and only arg of build_project_spec)
        let repr = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;

        let repr_text = format!("{repr}");
        let cmakelists_map = collect_cmakelists_map(repr);
        let source_files = source_tree_files(repr);

        let llm = BuildAnalyzerLLM::build(&config)?;
        let llm_response = llm.analyze_project(&repr_text, &cmakelists_map)?;

        let mut targets: HashMap<PathBuf, TargetSpec> = HashMap::new();
        let mut target_order = Vec::with_capacity(llm_response.targets.len());
        for target in llm_response.targets {
            let artifact_path = PathBuf::from(target.artifact);
            target_order.push(artifact_path.clone());

            let mut sources = RawDir::default();
            for source_path in target
                .sources
                .into_iter()
                .map(PathBuf::from)
                .filter(|p| has_allowed_source_extension(p))
                .filter(|p| source_files.contains(p))
            {
                let source_contents = repr
                    .dir
                    .get_file(&source_path)
                    .map_err(|e| {
                        format!(
                            "failed to read source file '{}' from raw source: {e}",
                            source_path.display()
                        )
                    })?
                    .clone();

                sources
                    .set_file(&source_path, source_contents)
                    .map_err(|e| {
                        format!(
                            "failed to insert source file '{}' into target source tree: {e}",
                            source_path.display()
                        )
                    })?;
            }

            targets.insert(
                artifact_path,
                TargetSpec {
                    kind: target.kind,
                    sources: RawSource { dir: sources },
                },
            );
        }

        info!("LLM response contains {} targets.", targets.len());
        let usage_totals = llm.usage_totals();
        info!(
            "Token usage [total] - prompt: {}, output: {}, total: {}",
            usage_totals.prompt_tokens, usage_totals.output_tokens, usage_totals.total_tokens
        );

        Ok(Box::new(ProjectSpec {
            targets,
            target_order,
        }))
    }
}

impl Tool for SelectPrimaryTarget {
    fn name(&self) -> &'static str {
        "select_primary_target"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let project_spec = context
            .ir_snapshot
            .get::<ProjectSpec>(inputs[0])
            .ok_or("No ProjectSpec representation found in IR")?;

        let artifact = project_spec
            .target_order
            .first()
            .ok_or("ProjectSpec has no targets")?
            .clone();
        let target = project_spec
            .targets
            .get(&artifact)
            .ok_or("Primary target missing from target map")?;

        Ok(Box::new(ProjectTarget {
            artifact,
            kind: target.kind,
            sources: clone_raw_source(&target.sources)?,
        }))
    }
}
