//! Translates C compilation units to Rust files using LLM.
//!
//! This module provides a dedicated pipeline for compile_commands.json mode where
//! each .c file is translated independently without JSON wrapper overhead.

use harvest_core::llm::{HarvestLLM, LLMConfig};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tempfile::TempDir;
use tracing::{debug, error, info, trace};
use walkdir::WalkDir;

const SYSTEM_PROMPT: &str = include_str!("system_prompt.txt");

/// Entry from compile_commands.json
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompileCommandsEntry {
    pub directory: String,
    pub file: String,
    pub command: Option<String>,
    pub arguments: Option<Vec<String>>,
}

/// Bundle of source files for a single compilation unit
#[derive(Debug)]
pub struct SourceBundle {
    /// The main .c source file
    pub main_source: (PathBuf, String),
    /// Related header files
    pub headers: Vec<(PathBuf, String)>,
    /// Temporary directory holding the files (must be kept alive)
    _temp_dir: TempDir,
}

/// Result of translating a single compilation unit
#[derive(Debug)]
pub struct TranslationResult {
    /// Original C source file path
    pub source_file: PathBuf,
    /// Generated Rust file path
    pub output_file: PathBuf,
    /// Size of generated Rust code in bytes
    pub output_size: usize,
}

/// Configuration for compilation unit translation
#[derive(Debug)]
pub struct TranslationConfig<'a> {
    pub llm_config: &'a LLMConfig,
    pub custom_prompt: Option<&'a Path>,
    pub parallel: bool,
    pub parallelism: usize,
}

/// Parse compile_commands.json file
pub fn parse_compile_commands(
    path: &Path,
) -> Result<Vec<CompileCommandsEntry>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let entries: Vec<CompileCommandsEntry> = serde_json::from_str(&content)?;
    Ok(entries)
}

/// Collect source files for a single compilation unit
///
/// This function:
/// 1. Parses #include directives in the .c file
/// 2. Collects only referenced header files from the project
/// 3. Ignores system headers
/// 4. Returns a bundle with the source and headers
pub fn collect_sources(
    entry: &CompileCommandsEntry,
    project_root: &Path,
) -> Result<SourceBundle, Box<dyn std::error::Error>> {
    // Resolve absolute path to .c file
    let abs_c = {
        let p = PathBuf::from(&entry.file);
        if p.is_absolute() {
            p
        } else {
            PathBuf::from(&entry.directory).join(p)
        }
    };

    // Create temporary directory
    let temp_dir = tempfile::tempdir()?;
    let out_root = temp_dir.path().to_path_buf();

    // Compute relative path for the .c file
    let rel_c = abs_c
        .strip_prefix(project_root)
        .unwrap_or_else(|_| Path::new(abs_c.file_name().unwrap_or_default()));

    // Copy .c file to temp directory
    let dest_c = out_root.join(rel_c);
    if let Some(parent) = dest_c.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&abs_c, &dest_c)?;

    // Parse #include directives to find referenced headers
    let mut include_names: HashSet<String> = HashSet::new();
    if let Ok(src) = fs::read_to_string(&abs_c) {
        for line in src.lines() {
            let line = line.trim();
            // Only parse #include "..." (local headers), not #include <...> (system headers)
            if let Some(rest) = line.strip_prefix("#include \"") {
                if let Some(end) = rest.find('"') {
                    include_names.insert(rest[..end].to_string());
                }
            }
        }
    }

    // Collect matching headers from project_root
    let mut headers = Vec::new();
    for entry in WalkDir::new(project_root)
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

        // Filter: only include headers that were referenced
        if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
            if !include_names.is_empty() && !include_names.contains(fname) {
                continue;
            }
        }

        // Copy header to temp directory
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or_else(|_| Path::new(path.file_name().unwrap()));
        let dest = out_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::copy(path, &dest).is_ok() {
            if let Ok(content) = fs::read_to_string(&dest) {
                headers.push((rel.to_path_buf(), content));
            }
        }
    }

    // Read the main source file
    let main_source_content = fs::read_to_string(&dest_c)?;

    Ok(SourceBundle {
        main_source: (rel_c.to_path_buf(), main_source_content),
        headers,
        _temp_dir: temp_dir,
    })
}

/// Translate a single compilation unit using LLM
pub fn translate_single_unit(
    entry: &CompileCommandsEntry,
    project_root: &Path,
    output_dir: &Path,
    config: &TranslationConfig,
) -> Result<TranslationResult, Box<dyn std::error::Error>> {
    // Step 1: Collect source files
    let bundle = collect_sources(entry, project_root)?;

    // Step 2: Build system prompt
    let system_prompt = if let Some(prompt_path) = config.custom_prompt {
        fs::read_to_string(prompt_path)?
    } else {
        SYSTEM_PROMPT.to_owned()
    };

    // Step 3: Build LLM client without structured output schema
    let llm = HarvestLLM::build(config.llm_config, None, &system_prompt)?;

    // Step 4: Assemble the LLM request
    #[derive(Serialize)]
    struct FileInput {
        path: String,
        contents: String,
    }

    #[derive(Serialize)]
    struct RequestBody {
        files: Vec<FileInput>,
    }

    let mut files = vec![FileInput {
        path: bundle.main_source.0.to_string_lossy().to_string(),
        contents: bundle.main_source.1.clone(),
    }];

    for (path, contents) in &bundle.headers {
        files.push(FileInput {
            path: path.to_string_lossy().to_string(),
            contents: contents.clone(),
        });
    }

    // Build request - rely on system prompt for output format instruction
    let request_json = serde_json::to_string(&RequestBody { files })?;
    trace!("Request JSON size: {} bytes", request_json.len());
    let request = vec![
        harvest_core::llm::ChatMessage::user()
            .content("Translate the following C source file to Rust:")
            .build(),
        harvest_core::llm::ChatMessage::user()
            .content(&request_json)
            .build(),
    ];

    // Step 5: Make the LLM call
    trace!("Translating {}", bundle.main_source.0.display());
    trace!("Making LLM call with {} messages", request.len());
    info!("Starting LLM translation for {}", entry.file);
    let response = llm.invoke(&request)?;
    info!("LLM translation completed for {}", entry.file);
    trace!("LLM responded with {} bytes", response.len());

    // Step 6: Determine output path
    let mut rel_rs = bundle.main_source.0.clone();
    rel_rs.set_extension("rs");
    let output_path = output_dir.join(&rel_rs);

    // Step 7: Write output file
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, &response)?;

    info!(
        "Translated {} to {} ({} bytes)",
        entry.file,
        output_path.display(),
        response.len()
    );

    Ok(TranslationResult {
        source_file: PathBuf::from(&entry.file),
        output_file: output_path,
        output_size: response.len(),
    })
}

/// Process an entire compile_commands.json file
///
/// This is the main entry point for compile_commands mode.
/// It processes each compilation unit independently and returns the results.
pub fn process_compile_commands(
    cc_path: &Path,
    project_root: &Path,
    output_dir: &Path,
    config: &TranslationConfig,
) -> Result<Vec<TranslationResult>, Box<dyn std::error::Error>> {
    let entries = parse_compile_commands(cc_path)?;

    if entries.is_empty() {
        return Err("compile_commands.json is empty".into());
    }

    info!(
        "Processing {} compilation units from {}",
        entries.len(),
        cc_path.display()
    );

    if config.parallel {
        info!("Processing in parallel (max {} threads)...", config.parallelism);

        // Build thread pool with limited parallelism
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(config.parallelism)
            .build()
            .expect("Failed to build thread pool");

        let errors = Mutex::new(Vec::new());

        let results: Vec<_> = pool.install(|| {
            entries
                .par_iter()
                .enumerate()
                .filter_map(|(idx, entry)| {
                    debug!(
                        "Processing {}/{}: {}",
                        idx + 1,
                        entries.len(),
                        entry.file
                    );

                    match translate_single_unit(entry, project_root, output_dir, config) {
                        Ok(result) => Some(result),
                        Err(e) => {
                            error!("Failed to translate {}: {}", entry.file, e);
                            errors.lock().unwrap().push((entry.file.clone(), e.to_string()));
                            None
                        }
                    }
                })
                .collect()
        });

        let errors = errors.into_inner().unwrap();
        if !errors.is_empty() {
            error!(
                "Translation completed with {} errors out of {} units",
                errors.len(),
                entries.len()
            );
            for (file, err) in &errors {
                error!("  {}: {}", file, err);
            }
        } else {
            info!(
                "Successfully translated all {} compilation units",
                entries.len()
            );
        }

        Ok(results)
    } else {
        info!("Processing sequentially...");

        let mut results = Vec::new();
        let mut errors = Vec::new();

        for (idx, entry) in entries.iter().enumerate() {
            debug!(
                "Processing {}/{}: {}",
                idx + 1,
                entries.len(),
                entry.file
            );

            match translate_single_unit(entry, project_root, output_dir, config) {
                Ok(result) => results.push(result),
                Err(e) => {
                    error!("Failed to translate {}: {}", entry.file, e);
                    errors.push((entry.file.clone(), e.to_string()));
                }
            }
        }

        if !errors.is_empty() {
            error!(
                "Translation completed with {} errors out of {} units",
                errors.len(),
                entries.len()
            );
            for (file, err) in &errors {
                error!("  {}: {}", file, err);
            }
        } else {
            info!(
                "Successfully translated all {} compilation units",
                entries.len()
            );
        }

        Ok(results)
    }
}

// Legacy Tool implementation (kept for backward compatibility if needed)
// This can be removed if the Tool interface is no longer needed

use full_source::RawSource;
use harvest_core::tools::{RunContext, Tool};
use harvest_core::{Id, Representation};

pub struct CompilationUnitToRustLlm;

impl Tool for CompilationUnitToRustLlm {
    fn name(&self) -> &'static str {
        "compilation_unit_to_rust_llm"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        let config = ToolConfig::deserialize(
            context
                .config
                .tools
                .get("compilation_unit_to_rust_llm")
                .unwrap(),
        )?;
        debug!("LLM Configuration {config:?}");

        // Get RawSource input
        let in_dir = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;

        // Build system prompt
        let system_prompt = config
            .prompt
            .as_ref()
            .map(|p| std::fs::read_to_string(p))
            .transpose()?
            .unwrap_or_else(|| SYSTEM_PROMPT.to_owned());

        // Build LLM client WITHOUT structured output schema
        let llm = HarvestLLM::build(&config.llm, None, &system_prompt)?;

        // Assemble the LLM request
        #[derive(Serialize)]
        struct FileInput {
            path: String,
            contents: String,
        }

        let files: Vec<FileInput> = in_dir
            .dir
            .files_recursive()
            .iter()
            .map(|(path, contents)| FileInput {
                path: path.to_string_lossy().to_string(),
                contents: String::from_utf8_lossy(contents).into(),
            })
            .collect();

        #[derive(Serialize)]
        struct RequestBody {
            files: Vec<FileInput>,
        }

        // Build request - rely on system prompt for output format instruction
        let request_json = serde_json::to_string(&RequestBody { files })?;
        let request = vec![
            harvest_core::llm::ChatMessage::user()
                .content("Translate the following C source file to Rust:")
                .build(),
            harvest_core::llm::ChatMessage::user()
                .content(&request_json)
                .build(),
        ];

        // Make the LLM call
        trace!("Making LLM call with {:?}", request);
        let response = llm.invoke(&request)?;
        trace!("LLM responded with {} bytes", response.len());

        // Get output path from config
        let output_path = config
            .output_path
            .ok_or("output_path not set in compilation_unit_to_rust_llm config")?;

        // Get source file path for diagnostics
        let source_file = in_dir
            .dir
            .files_recursive()
            .iter()
            .find(|(p, _)| p.ends_with(".c"))
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| "unknown.c".into());

        info!(
            "Translated {} to {} ({} bytes)",
            source_file.display(),
            output_path,
            response.len()
        );

        Ok(Box::new(SingleRustFile {
            path: PathBuf::from(output_path),
            contents: response,
            source_file: PathBuf::from(source_file),
        }))
    }
}

/// Represents a single translated Rust file (for Tool interface)
#[derive(Debug, Clone)]
pub struct SingleRustFile {
    pub path: PathBuf,
    pub contents: String,
    pub source_file: PathBuf,
}

impl std::fmt::Display for SingleRustFile {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Single Rust file: {} (from {}, {} bytes)",
            self.path.display(),
            self.source_file.display(),
            self.contents.len()
        )
    }
}

impl Representation for SingleRustFile {
    fn name(&self) -> &'static str {
        "single_rust_file"
    }

    fn materialize(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, &self.contents)
    }
}

#[derive(Debug, Deserialize)]
struct ToolConfig {
    #[serde(flatten)]
    pub llm: LLMConfig,
    pub prompt: Option<PathBuf>,
    pub output_path: Option<String>,
}
