//! Deterministic single-pass scanner that turns a `CMakeLists.txt` (joined to
//! logical statements) and a `configuration.json` into a [`BuildConfigIR`].
//!
//! The recognized patterns are documented on each `handle_*` helper; anything
//! we don't recognize is logged as a warning and skipped -- the goal is a
//! best-effort projection, never a hard error.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use harvest_core::fs::RawDir;
use serde::Deserialize;
use serde_json::Value;
use tracing::warn;

use crate::ir::{
    BuildConfigIR, ConditionalTarget, ConfigVarKind, ConfigVariable, DefineKind, DefineMapping,
    SourceSelection, SourceVariant, SubdirSelection, SubdirVariant,
};

/// Top-level entry point. Given the project's `RawDir`, produce a
/// [`BuildConfigIR`]. Never returns an error: if anything is missing the
/// caller gets an `is_empty` IR.
pub fn scan(dir: &RawDir) -> BuildConfigIR {
    let Some(config_bytes) = dir.get_file("configuration.json").ok() else {
        return BuildConfigIR {
            is_empty: true,
            ..Default::default()
        };
    };
    let Some(config) = parse_configuration_json(config_bytes) else {
        return BuildConfigIR {
            is_empty: true,
            ..Default::default()
        };
    };
    if config.variables.is_empty() {
        return BuildConfigIR {
            is_empty: true,
            ..Default::default()
        };
    }

    // Reserve all the known files (relative paths) so we can validate
    // source-selection variant existence.
    let files: HashSet<PathBuf> = dir.files_recursive().into_iter().map(|(p, _)| p).collect();

    // The CMake side. Missing CMakeLists is non-fatal: we still know the
    // variable inventory.
    let scanned = if dir.get_file("CMakeLists.txt").is_ok() {
        scan_cmake(dir, &config, &files)
    } else {
        warn!("build_config: CMakeLists.txt missing; emitting variable inventory only");
        ScannedCmake::default()
    };

    let variables = build_variables(&config, &scanned.var_defaults);

    BuildConfigIR {
        variables,
        defines: scanned.defines,
        source_selections: scanned.source_selections,
        conditional_targets: scanned.conditional_targets,
        subdir_selections: scanned.subdir_selections,
        is_empty: false,
    }
}

/// Output of the CMake scanner. Contains the partial-IR fields plus the
/// CMake-derived per-variable defaults that need to be merged with
/// `configuration.json` before producing `BuildConfigIR::variables`.
#[derive(Default)]
struct ScannedCmake {
    defines: Vec<DefineMapping>,
    source_selections: Vec<SourceSelection>,
    conditional_targets: Vec<ConditionalTarget>,
    subdir_selections: Vec<SubdirSelection>,
    /// Variable name -> default value parsed from CMake (`set(... CACHE ...)`
    /// or `option(...)`).
    var_defaults: HashMap<String, Option<String>>,
}

/// Holds what we managed to extract from `configuration.json`. Each variable
/// keeps its raw JSON values so the scanner can decide kind ad-hoc.
struct Configuration {
    variables: BTreeMap<String, Vec<Value>>,
}

fn parse_configuration_json(bytes: &[u8]) -> Option<Configuration> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        configurable_variables: BTreeMap<String, Vec<Value>>,
    }
    match serde_json::from_slice::<Raw>(bytes) {
        Ok(raw) => Some(Configuration {
            variables: raw.configurable_variables,
        }),
        Err(error) => {
            warn!("build_config: failed to parse configuration.json: {error}");
            None
        }
    }
}

/// Build the canonical `Vec<ConfigVariable>` from configuration.json. Defaults
/// detected in CMake are merged in via `cmake_defaults`.
fn build_variables(
    config: &Configuration,
    cmake_defaults: &HashMap<String, Option<String>>,
) -> Vec<ConfigVariable> {
    let mut out = Vec::with_capacity(config.variables.len());
    for (name, raw_values) in &config.variables {
        let mut values: Vec<String> = Vec::with_capacity(raw_values.len());
        let mut all_bool = !raw_values.is_empty();
        let mut all_numeric = !raw_values.is_empty();
        for v in raw_values {
            match v {
                Value::Bool(b) => {
                    values.push(if *b { "true".into() } else { "false".into() });
                    all_numeric = false;
                }
                Value::String(s) => {
                    if s.parse::<i64>().is_err() {
                        all_numeric = false;
                    }
                    all_bool = false;
                    values.push(s.clone());
                }
                Value::Number(n) => {
                    all_bool = false;
                    values.push(n.to_string());
                }
                other => {
                    warn!(
                        "build_config: unsupported value `{other}` for variable `{name}`; skipping"
                    );
                    all_bool = false;
                    all_numeric = false;
                }
            }
        }

        let kind = if all_bool {
            ConfigVarKind::Boolean
        } else {
            ConfigVarKind::Enum {
                values,
                numeric: all_numeric,
            }
        };

        let default = cmake_defaults.get(name).cloned().flatten();
        out.push(ConfigVariable {
            name: name.clone(),
            kind,
            default,
        });
    }
    out
}

#[derive(Debug)]
struct Statement {
    command: String,
    args: Vec<String>,
}

fn scan_cmake(dir: &RawDir, config: &Configuration, files: &HashSet<PathBuf>) -> ScannedCmake {
    let known_vars: HashSet<&str> = config.variables.keys().map(String::as_str).collect();
    // Map each configurable variable to the stringified values declared in
    // `configuration.json`. Used by `handle_add_subdirectory` to enumerate the
    // subtrees behind `add_subdirectory(${VAR})`. The stringification mirrors
    // `build_variables` (`true`/`false` for booleans, decimal for numbers).
    let var_values: HashMap<String, Vec<String>> = config
        .variables
        .iter()
        .map(|(k, raw)| {
            let values: Vec<String> = raw
                .iter()
                .filter_map(|v| match v {
                    Value::Bool(b) => Some(if *b {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    }),
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect();
            (k.clone(), values)
        })
        .collect();

    let mut out = ScannedCmake::default();
    let mut source_lists: HashMap<String, Vec<String>> = HashMap::new();
    let mut composed_vars: HashMap<String, String> = HashMap::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    scan_cmake_file(
        dir,
        Path::new(""),
        &known_vars,
        &var_values,
        files,
        &mut out,
        &mut source_lists,
        &mut composed_vars,
        &mut visited,
    );

    // Pass 2: rewrite Bare defines whose variable is actually a composed
    // CMake var into `Composed`.
    rewrite_composed_defines(&mut out.defines, &composed_vars, &known_vars);

    out
}

/// Scan the `CMakeLists.txt` at `<dir>/<rel_dir>/CMakeLists.txt`, recursing
/// into literal `add_subdirectory(<path>)` invocations. All source paths
/// recorded by handlers are normalized relative to the project root via
/// [`normalize_join`].
#[allow(clippy::too_many_arguments)]
fn scan_cmake_file(
    dir: &RawDir,
    rel_dir: &Path,
    known_vars: &HashSet<&str>,
    var_values: &HashMap<String, Vec<String>>,
    files: &HashSet<PathBuf>,
    out: &mut ScannedCmake,
    source_lists: &mut HashMap<String, Vec<String>>,
    composed_vars: &mut HashMap<String, String>,
    visited: &mut HashSet<PathBuf>,
) {
    if !visited.insert(rel_dir.to_path_buf()) {
        return;
    }
    let cmake_path = rel_dir.join("CMakeLists.txt");
    let Ok(bytes) = dir.get_file(&cmake_path) else {
        warn!(
            "build_config: add_subdirectory points at `{}` but `{}` is missing; skipping",
            rel_dir.display(),
            cmake_path.display(),
        );
        return;
    };
    let text = String::from_utf8_lossy(bytes);
    let statements = join_statements(&text);

    // Stack of `if`-guard variables, local to this file. `Some(name)` means we
    // recognized the guard variable; `None` means we entered an `if(...)` we
    // don't recognize and should not treat anything inside as gated by a known
    // variable.
    let mut if_stack: Vec<Option<String>> = Vec::new();

    for stmt in &statements {
        // Track if/endif depth first, regardless of recognition.
        match stmt.command.as_str() {
            "if" => {
                if_stack.push(extract_if_gate(&stmt.args, known_vars));
                continue;
            }
            "elseif" => {
                if let Some(top) = if_stack.last_mut() {
                    *top = extract_if_gate(&stmt.args, known_vars);
                }
                continue;
            }
            "else" => continue,
            "endif" => {
                if_stack.pop();
                continue;
            }
            _ => {}
        }

        // Innermost recognized gate wins.
        let inside_gate: Option<&str> = if_stack.iter().rev().find_map(|opt| opt.as_deref());

        match stmt.command.as_str() {
            "set" => {
                handle_set(
                    &stmt.args,
                    known_vars,
                    rel_dir,
                    out,
                    source_lists,
                    composed_vars,
                );
            }
            "option" => {
                handle_option(&stmt.args, known_vars, out);
            }
            "add_compile_definitions" => {
                handle_add_compile_definitions(&stmt.args, known_vars, out);
            }
            "add_definitions" => {
                handle_add_definitions(&stmt.args, known_vars, out, inside_gate);
            }
            "add_library" | "add_executable" => {
                handle_target_definition(
                    &stmt.args,
                    known_vars,
                    source_lists,
                    rel_dir,
                    files,
                    inside_gate,
                    out,
                );
            }
            "target_compile_definitions" => {
                handle_target_compile_definitions(&stmt.args, inside_gate, out);
            }
            "add_subdirectory" => {
                handle_add_subdirectory(
                    &stmt.args,
                    dir,
                    rel_dir,
                    known_vars,
                    var_values,
                    files,
                    out,
                    source_lists,
                    composed_vars,
                    visited,
                );
            }
            _ => {}
        }
    }
}

/// Handle `add_subdirectory(<path>)`.
///
/// - **Literal path**: recursively scan `<rel_dir>/<path>/CMakeLists.txt`.
///   Source paths defined inside that file are normalized to project-relative
///   form via [`normalize_join`].
/// - **`${VAR}` reference**: enumerate the values of `VAR` declared in
///   `configuration.json`. For each value whose `<rel_dir>/<value>/CMakeLists.txt`
///   exists, scan it with a cloned snapshot of the surrounding source-list /
///   composed-variable state and collect the IR fragment into a
///   [`SubdirSelection`]. Variants do not see one another's defines, source
///   lists, or targets.
#[allow(clippy::too_many_arguments)]
fn handle_add_subdirectory(
    args: &[String],
    dir: &RawDir,
    rel_dir: &Path,
    known_vars: &HashSet<&str>,
    var_values: &HashMap<String, Vec<String>>,
    files: &HashSet<PathBuf>,
    out: &mut ScannedCmake,
    source_lists: &mut HashMap<String, Vec<String>>,
    composed_vars: &mut HashMap<String, String>,
    visited: &mut HashSet<PathBuf>,
) {
    let Some(first) = args.first() else { return };
    let text = arg_text(first);

    // `add_subdirectory(${VAR})` -- one subtree per value of VAR.
    if let Some(var) = as_single_var_ref(&text)
        && known_vars.contains(var)
    {
        let Some(values) = var_values.get(var) else {
            warn!(
                "build_config: `add_subdirectory(${{{var}}})` at `{}`, but `{var}` \
                 has no values in `configuration.json`; skipping",
                rel_dir.display(),
            );
            return;
        };
        let mut variants: Vec<SubdirVariant> = Vec::new();
        for value in values {
            let Some(child) = normalize_join(rel_dir, Path::new(value)) else {
                continue;
            };
            // Variant subtrees must exist on disk; skip silently if not.
            if dir.get_file(child.join("CMakeLists.txt")).is_err() {
                continue;
            }
            // Fresh per-variant state, seeded from the surrounding scope so
            // variants can see (but not mutate) outer accumulators.
            let mut variant_out = ScannedCmake::default();
            let mut variant_source_lists = source_lists.clone();
            let mut variant_composed_vars = composed_vars.clone();
            let mut variant_visited: HashSet<PathBuf> = HashSet::new();
            scan_cmake_file(
                dir,
                &child,
                known_vars,
                var_values,
                files,
                &mut variant_out,
                &mut variant_source_lists,
                &mut variant_composed_vars,
                &mut variant_visited,
            );
            // Resolve composed defines within the variant only.
            rewrite_composed_defines(&mut variant_out.defines, &variant_composed_vars, known_vars);
            variants.push(SubdirVariant {
                value: value.clone(),
                path: child,
                defines: variant_out.defines,
                source_selections: variant_out.source_selections,
                conditional_targets: variant_out.conditional_targets,
                subdir_selections: variant_out.subdir_selections,
            });
        }
        if !variants.is_empty() {
            out.subdir_selections.push(SubdirSelection {
                driving_var: var.to_string(),
                variants,
            });
        } else {
            warn!(
                "build_config: `add_subdirectory(${{{var}}})` at `{}` matched no \
                 subdirectory with a `CMakeLists.txt`; nothing recorded",
                rel_dir.display(),
            );
        }
        return;
    }

    // Literal path.
    let Some(child) = normalize_join(rel_dir, Path::new(&text)) else {
        warn!(
            "build_config: `add_subdirectory({text})` at `{}` escapes the project \
             root; skipping",
            rel_dir.display(),
        );
        return;
    };
    scan_cmake_file(
        dir,
        &child,
        known_vars,
        var_values,
        files,
        out,
        source_lists,
        composed_vars,
        visited,
    );
}

/// Prefix a source path declared inside `<rel_dir>/CMakeLists.txt` with
/// `rel_dir` and normalize the result. `${VAR}` placeholders pass through
/// untouched -- they are resolved by `enumerate_variants` against the file
/// set, which already holds project-relative paths.
fn prefix_source_path(rel_dir: &Path, src: &str) -> String {
    if rel_dir.as_os_str().is_empty() {
        return src.to_string();
    }
    let path = Path::new(src);
    match normalize_join(rel_dir, path) {
        Some(joined) => joined.to_string_lossy().into_owned(),
        // If normalization fails (escapes the project root) we keep the
        // original spelling so downstream consumers can still see what the
        // source declared.
        None => src.to_string(),
    }
}

/// Join `rel_dir` and `path`, normalizing `.` and `..` components. Returns
/// `None` if the result escapes the project root.
fn normalize_join(rel_dir: &Path, path: &Path) -> Option<PathBuf> {
    let mut segments: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in rel_dir.components().chain(path.components()) {
        match comp {
            Component::CurDir => {}
            Component::Normal(s) => segments.push(s),
            Component::ParentDir => {
                segments.pop()?;
            }
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    let mut out = PathBuf::new();
    for s in segments {
        out.push(s);
    }
    Some(out)
}

/// Joins multi-line `(...)` statements into single logical statements.
///
/// CMake commands have the shape `name(args...)`. Arguments may span
/// multiple lines. This function returns one [`Statement`] per logical
/// invocation. Comments (`#...`) are stripped first.
fn join_statements(text: &str) -> Vec<Statement> {
    let stripped = strip_comments(text);

    let mut out = Vec::new();
    let bytes = stripped.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read command name (identifier).
        let cmd_start = i;
        while i < bytes.len() && is_cmake_ident_char(bytes[i]) {
            i += 1;
        }
        if i == cmd_start {
            // Not a command-start. Advance to next char and retry.
            i += 1;
            continue;
        }
        let command = stripped[cmd_start..i].to_string();
        // Skip whitespace until '('.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'(' {
            // Not a real command; skip.
            continue;
        }
        i += 1; // Skip '('.
        // Read arguments until matching ')'.
        let arg_start = i;
        let mut depth: usize = 1;
        let mut in_quote = false;
        while i < bytes.len() && depth > 0 {
            let c = bytes[i];
            if in_quote {
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == b'"' {
                    in_quote = false;
                }
                i += 1;
                continue;
            }
            match c {
                b'"' => {
                    in_quote = true;
                    i += 1;
                }
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b')' => {
                    depth -= 1;
                    i += 1;
                }
                _ => {
                    i += 1;
                }
            }
        }
        // i now points one past the matching `)`. The args span is
        // `arg_start..i-1`.
        let arg_end = if depth == 0 { i - 1 } else { i };
        let args = tokenize_args(&stripped[arg_start..arg_end]);
        out.push(Statement { command, args });
    }
    out
}

fn is_cmake_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Tokenize the inside of a `(...)` into individual arguments. Honors
/// double-quoted strings (preserving inner whitespace and backslash escapes).
fn tokenize_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'"' {
            // Quoted string. Preserve the leading quote so callers can
            // distinguish quoted from unquoted arguments.
            let start = i;
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(s[start..i].to_string());
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(s[start..i].to_string());
        }
    }
    out
}

/// Strip `#`-comments outside of double-quoted strings.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_quote = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_quote {
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(c as char);
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_quote = false;
            }
            out.push(c as char);
            i += 1;
            continue;
        }
        if c == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'"' {
            in_quote = true;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// Strip the outer double quotes (and unescape `\"`) from a quoted CMake
/// argument. Returns `None` if the argument is not quoted.
fn unquote(arg: &str) -> Option<String> {
    let bytes = arg.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return None;
    }
    let inner = &arg[1..arg.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Returns the contents of an argument with any leading/trailing double-quote
/// stripped. Useful for the simple/common case of a "literal" string argument.
fn arg_text(arg: &str) -> String {
    unquote(arg).unwrap_or_else(|| arg.to_string())
}

/// Extract a `${VAR}` reference if the entire string is a single ref.
fn as_single_var_ref(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.starts_with("${") && s.ends_with('}') && s.len() > 3 {
        let inner = &s[2..s.len() - 1];
        if inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Some(inner);
        }
    }
    None
}

/// Find every `${VAR}` reference within a string. Returns them in order of
/// appearance.
fn find_var_refs(s: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let start = i;
            i += 2;
            let name_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'}' {
                let name = s[name_start..i].to_string();
                let end = i + 1;
                out.push((start, end, name));
                i = end;
                continue;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Extract the gate variable from an `if(...)`-like argument list. Returns
/// `Some(var)` only when the argument names a known configurable variable.
fn extract_if_gate(args: &[String], known_vars: &HashSet<&str>) -> Option<String> {
    let first = args.first()?;
    let name = arg_text(first);
    if known_vars.contains(name.as_str()) {
        return Some(name);
    }
    None
}

fn handle_set(
    args: &[String],
    known_vars: &HashSet<&str>,
    rel_dir: &Path,
    out: &mut ScannedCmake,
    source_lists: &mut HashMap<String, Vec<String>>,
    composed_vars: &mut HashMap<String, String>,
) {
    if args.is_empty() {
        return;
    }
    let name = arg_text(&args[0]);

    // `set(VAR "value" CACHE STRING "doc")` -> variable default + Enum kind.
    if known_vars.contains(name.as_str()) && args.iter().any(|a| arg_text(a) == "CACHE") {
        let default = args.get(1).map(|s| arg_text(s)).filter(|s| !s.is_empty());
        out.var_defaults.insert(name.clone(), default);
        return;
    }

    // `set(CMAKE_C_FLAGS "${CMAKE_C_FLAGS} -DFOO=${VAR}")` -> DefineMapping
    // (Bare or Composed) for every `-DNAME=...` in the rhs.
    if name == "CMAKE_C_FLAGS" || name == "CMAKE_CXX_FLAGS" {
        if let Some(value) = args.get(1).map(|s| arg_text(s)) {
            extract_defines_from_flags(&value, known_vars, composed_vars, &mut out.defines);
        }
        return;
    }

    // `set(NAME_SOURCES path/with_${VAR}.c)` -> record for later targeting.
    // Source paths are normalized relative to the project root so a path in
    // `lib/CMakeLists.txt` like `src/foo.c` becomes `lib/src/foo.c`.
    if name.ends_with("_SOURCES") {
        let values: Vec<String> = args
            .iter()
            .skip(1)
            .map(|s| prefix_source_path(rel_dir, &arg_text(s)))
            .collect();
        source_lists.insert(name.clone(), values);
        return;
    }

    // `set(BUILD_PROFILE "${BACKEND}_${WORD_SIZE}")` -> remember composed value
    // so we can rewrite its later use.
    if let Some(value) = args.get(1).map(|s| arg_text(s))
        && !value.is_empty()
        && find_var_refs(&value)
            .iter()
            .any(|(_, _, v)| known_vars.contains(v.as_str()))
    {
        composed_vars.insert(name.clone(), value);
    }
}

fn handle_option(args: &[String], known_vars: &HashSet<&str>, out: &mut ScannedCmake) {
    if args.is_empty() {
        return;
    }
    let name = arg_text(&args[0]);
    if !known_vars.contains(name.as_str()) {
        return;
    }
    // `option(VAR "doc" ON|OFF)` -- default is the 3rd arg if present.
    let default = match args.get(2).map(|s| arg_text(s)).as_deref() {
        Some("ON" | "TRUE" | "YES" | "1") => Some("true".to_string()),
        Some("OFF" | "FALSE" | "NO" | "0") => Some("false".to_string()),
        _ => None,
    };
    out.var_defaults.insert(name, default);
}

fn handle_add_compile_definitions(
    args: &[String],
    known_vars: &HashSet<&str>,
    out: &mut ScannedCmake,
) {
    for arg in args {
        let text = arg_text(arg);
        // Strip a leading `-D` if present (CMake accepts both forms).
        let body = text.strip_prefix("-D").unwrap_or(&text);
        let Some((name, value)) = body.split_once('=') else {
            continue;
        };
        let c_name = name.trim().to_string();
        let value = value.trim();
        // QuotedString: "${VAR}" (after the equals).
        if let Some(inner) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
            && let Some(var) = as_single_var_ref(inner)
            && known_vars.contains(var)
        {
            out.defines.push(DefineMapping {
                c_name,
                kind: DefineKind::QuotedString {
                    var: var.to_string(),
                },
                source_vars: vec![var.to_string()],
            });
            continue;
        }
        // Bare ${VAR}
        if let Some(var) = as_single_var_ref(value)
            && known_vars.contains(var)
        {
            out.defines.push(DefineMapping {
                c_name,
                kind: DefineKind::Bare {
                    var: var.to_string(),
                },
                source_vars: vec![var.to_string()],
            });
            continue;
        }
        // Composed: NAME=${X}_${Y}
        let refs = find_var_refs(value);
        let known_refs: Vec<String> = refs
            .iter()
            .filter(|(_, _, v)| known_vars.contains(v.as_str()))
            .map(|(_, _, v)| v.clone())
            .collect();
        if known_refs.len() >= 2 {
            out.defines.push(DefineMapping {
                c_name,
                kind: DefineKind::Composed {
                    template: substitute_var_template(value, known_vars),
                },
                source_vars: known_refs,
            });
        }
    }
}

fn handle_add_definitions(
    args: &[String],
    known_vars: &HashSet<&str>,
    out: &mut ScannedCmake,
    inside_gate: Option<&str>,
) {
    for arg in args {
        let text = arg_text(arg);
        let body = text.strip_prefix("-D").unwrap_or(&text);
        // `add_definitions(-DTAG)` under an `if(VAR STREQUAL "x")` -> GatedFlag.
        if !body.contains('=') {
            if let Some(gate) = inside_gate {
                out.defines.push(DefineMapping {
                    c_name: body.to_string(),
                    kind: DefineKind::GatedFlag {
                        gate_var: gate.to_string(),
                    },
                    source_vars: vec![gate.to_string()],
                });
            }
            continue;
        }
        // `add_definitions(-DNAME=${VAR})`
        let Some((name, value)) = body.split_once('=') else {
            continue;
        };
        let c_name = name.trim().to_string();
        let value = value.trim();
        if let Some(var) = as_single_var_ref(value)
            && known_vars.contains(var)
        {
            out.defines.push(DefineMapping {
                c_name,
                kind: DefineKind::Bare {
                    var: var.to_string(),
                },
                source_vars: vec![var.to_string()],
            });
        }
    }
}

/// Replace each `${VAR}` (where VAR in known_vars) with `{VAR}` to form a
/// Composed template. Other `${...}` references are left untouched.
fn substitute_var_template(value: &str, known_vars: &HashSet<&str>) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            let name_start = i + 2;
            let mut j = name_start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' {
                let name = &value[name_start..j];
                if known_vars.contains(name) {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                } else {
                    out.push_str(&value[i..=j]);
                }
                i = j + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Extract `-DNAME=...` defines from a `set(CMAKE_*FLAGS ...)` rhs.
///
/// `composed_vars` carries previously-recorded composed CMake variables (e.g.
/// `BUILD_PROFILE` -> `${BACKEND}_${WORD_SIZE}`). If a single `${VAR}` ref
/// points at one of those, we emit a `Composed` define right away rather
/// than relying on the second pass.
fn extract_defines_from_flags(
    value: &str,
    known_vars: &HashSet<&str>,
    composed_vars: &HashMap<String, String>,
    defines: &mut Vec<DefineMapping>,
) {
    for token in value.split_ascii_whitespace() {
        if !token.starts_with("-D") {
            continue;
        }
        let body = &token[2..];
        let Some((name, val)) = body.split_once('=') else {
            continue;
        };
        let c_name = name.trim().to_string();
        let val = val.trim();
        // Single ${VAR}: known -> Bare, otherwise check composed_vars.
        if let Some(var) = as_single_var_ref(val) {
            if known_vars.contains(var) {
                defines.push(DefineMapping {
                    c_name,
                    kind: DefineKind::Bare {
                        var: var.to_string(),
                    },
                    source_vars: vec![var.to_string()],
                });
                continue;
            }
            if let Some(composed_value) = composed_vars.get(var) {
                let known_refs: Vec<String> = find_var_refs(composed_value)
                    .iter()
                    .filter(|(_, _, v)| known_vars.contains(v.as_str()))
                    .map(|(_, _, v)| v.clone())
                    .collect();
                if known_refs.len() >= 2 {
                    defines.push(DefineMapping {
                        c_name,
                        kind: DefineKind::Composed {
                            template: substitute_var_template(composed_value, known_vars),
                        },
                        source_vars: known_refs,
                    });
                    continue;
                }
            }
            continue;
        }
        // Composed in the same token (e.g. `-DBUILD=${X}_${Y}` directly).
        let refs = find_var_refs(val);
        let known_refs: Vec<String> = refs
            .iter()
            .filter(|(_, _, v)| known_vars.contains(v.as_str()))
            .map(|(_, _, v)| v.clone())
            .collect();
        if known_refs.len() >= 2 {
            defines.push(DefineMapping {
                c_name,
                kind: DefineKind::Composed {
                    template: substitute_var_template(val, known_vars),
                },
                source_vars: known_refs,
            });
        }
    }
}

fn handle_target_definition(
    args: &[String],
    known_vars: &HashSet<&str>,
    source_lists: &HashMap<String, Vec<String>>,
    rel_dir: &Path,
    files: &HashSet<PathBuf>,
    inside_gate: Option<&str>,
    out: &mut ScannedCmake,
) {
    if args.is_empty() {
        return;
    }
    let target = arg_text(&args[0]);

    // Collect file/source arguments, expanding `${NAME_SOURCES}` references.
    let mut file_args: Vec<String> = Vec::new();
    let mut driving_var: Option<String> = None;

    for arg in args.iter().skip(1) {
        let text = arg_text(arg);
        if matches!(
            text.as_str(),
            "STATIC" | "SHARED" | "MODULE" | "INTERFACE" | "OBJECT" | "EXCLUDE_FROM_ALL"
        ) {
            continue;
        }
        if let Some(var) = as_single_var_ref(&text)
            && let Some(sources) = source_lists.get(var)
        {
            for source in sources {
                for (_, _, name) in find_var_refs(source) {
                    if known_vars.contains(name.as_str()) {
                        driving_var = Some(name);
                    }
                }
                file_args.push(source.clone());
            }
            continue;
        }
        // Direct path (e.g. `src/main.c` or `src/extra.c`).
        // Resolved relative to the project root via `rel_dir` so a path in
        // `lib/CMakeLists.txt` like `src/main.c` becomes `lib/src/main.c`.
        file_args.push(prefix_source_path(rel_dir, &text));
    }

    // If a known variable expands within any file path, treat this as a
    // SourceSelection.
    if let Some(driving_var) = driving_var.clone() {
        let pattern = file_args
            .iter()
            .find(|s| find_var_refs(s).iter().any(|(_, _, v)| v == &driving_var))
            .cloned()
            .unwrap_or_default();
        let variants = enumerate_variants(&pattern, &driving_var, files);
        if !variants.is_empty() {
            out.source_selections.push(SourceSelection {
                target: target.clone(),
                driving_var,
                variants,
            });
            return;
        }
    }

    // Otherwise, if the target is inside an `if(known_var)`, record as a
    // conditional target. Files are taken verbatim (and filtered by
    // existence).
    if let Some(gate) = inside_gate {
        let files_resolved: Vec<PathBuf> = file_args
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| files.contains(p))
            .collect();
        if !files_resolved.is_empty() {
            out.conditional_targets.push(ConditionalTarget {
                target,
                gate_var: gate.to_string(),
                files: files_resolved,
            });
        }
    }
}

/// Given a pattern like `src/backend_${BACKEND}.c`, enumerate concrete
/// `(value, files)` pairs by probing every known file in the project.
fn enumerate_variants(
    pattern: &str,
    driving_var: &str,
    files: &HashSet<PathBuf>,
) -> Vec<SourceVariant> {
    let needle = format!("${{{driving_var}}}");
    let Some(idx) = pattern.find(&needle) else {
        return Vec::new();
    };
    let prefix = &pattern[..idx];
    let suffix = &pattern[idx + needle.len()..];

    let mut values_seen: BTreeSet<String> = BTreeSet::new();
    let mut by_value: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();

    for path in files {
        let Some(s) = path.to_str() else { continue };
        if let Some(rest) = s.strip_prefix(prefix)
            && let Some(value) = rest.strip_suffix(suffix)
            && !value.is_empty()
            && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            values_seen.insert(value.to_string());
            by_value
                .entry(value.to_string())
                .or_default()
                .push(path.clone());
        }
    }

    values_seen
        .into_iter()
        .map(|value| SourceVariant {
            files: by_value.remove(&value).unwrap_or_default(),
            value,
        })
        .collect()
}

fn handle_target_compile_definitions(
    args: &[String],
    inside_gate: Option<&str>,
    out: &mut ScannedCmake,
) {
    let Some(gate) = inside_gate else { return };
    // target_compile_definitions(TARGET <KEYWORD> NAME [NAME ...])
    for arg in args.iter().skip(1) {
        let text = arg_text(arg);
        if matches!(text.as_str(), "PRIVATE" | "PUBLIC" | "INTERFACE") {
            continue;
        }
        // Skip `NAME=value` forms in this minimal pass; the v1 scope only
        // covers bare flag definitions inside if-guards.
        if text.contains('=') {
            continue;
        }
        // Avoid duplicating the same `c_name` for the same gate.
        if out.defines.iter().any(|d| {
            d.c_name == text
                && matches!(&d.kind, DefineKind::GatedFlag { gate_var } if gate_var == gate)
        }) {
            continue;
        }
        out.defines.push(DefineMapping {
            c_name: text,
            kind: DefineKind::GatedFlag {
                gate_var: gate.to_string(),
            },
            source_vars: vec![gate.to_string()],
        });
    }
}

/// Pass 2 over the defines: if a `Bare { var }` actually references a CMake
/// composed variable, rewrite it as `Composed { template }`.
fn rewrite_composed_defines(
    defines: &mut [DefineMapping],
    composed_vars: &HashMap<String, String>,
    known_vars: &HashSet<&str>,
) {
    for define in defines.iter_mut() {
        let DefineKind::Bare { var } = &define.kind else {
            continue;
        };
        let Some(value) = composed_vars.get(var) else {
            continue;
        };
        let refs = find_var_refs(value);
        let known_refs: Vec<String> = refs
            .iter()
            .filter(|(_, _, v)| known_vars.contains(v.as_str()))
            .map(|(_, _, v)| v.clone())
            .collect();
        if known_refs.len() < 2 {
            continue;
        }
        define.kind = DefineKind::Composed {
            template: substitute_var_template(value, known_vars),
        };
        define.source_vars = known_refs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&'static str]) -> HashSet<&'static str> {
        names.iter().copied().collect()
    }

    // ---------- character classes & low-level scanners ----------

    #[test]
    fn is_cmake_ident_char_accepts_alnum_and_underscore() {
        for c in b'a'..=b'z' {
            assert!(is_cmake_ident_char(c));
        }
        for c in b'A'..=b'Z' {
            assert!(is_cmake_ident_char(c));
        }
        for c in b'0'..=b'9' {
            assert!(is_cmake_ident_char(c));
        }
        assert!(is_cmake_ident_char(b'_'));
    }

    #[test]
    fn is_cmake_ident_char_rejects_symbols_and_whitespace() {
        for c in [b' ', b'\t', b'\n', b'-', b'.', b'(', b')', b'$', b'{', b'}'] {
            assert!(!is_cmake_ident_char(c), "should reject 0x{c:02x}");
        }
    }

    // ---------- strip_comments ----------

    #[test]
    fn strip_comments_removes_full_line_comment() {
        let out = strip_comments("# only a comment\nset(X 1)\n");
        assert_eq!(out, "\nset(X 1)\n");
    }

    #[test]
    fn strip_comments_removes_trailing_comment() {
        let out = strip_comments("set(X 1) # trailing\n");
        assert_eq!(out, "set(X 1) \n");
    }

    #[test]
    fn strip_comments_preserves_hash_inside_double_quotes() {
        let out = strip_comments(r#"set(X "a#b")"#);
        assert_eq!(out, r#"set(X "a#b")"#);
    }

    #[test]
    fn strip_comments_handles_escaped_quote_inside_string() {
        // Inside the quoted region, `\"` does NOT close the string, so the `#`
        // remains inside the quote and is preserved.
        let out = strip_comments(r##"set(X "a\"#b")"##);
        assert_eq!(out, r##"set(X "a\"#b")"##);
    }

    // ---------- tokenize_args ----------

    #[test]
    fn tokenize_args_splits_on_whitespace() {
        assert_eq!(
            tokenize_args("A B C"),
            vec!["A".to_string(), "B".to_string(), "C".to_string()]
        );
    }

    #[test]
    fn tokenize_args_preserves_outer_quotes_on_quoted_args() {
        assert_eq!(
            tokenize_args(r#"NAME "a b c" TAIL"#),
            vec![
                "NAME".to_string(),
                r#""a b c""#.to_string(),
                "TAIL".to_string()
            ]
        );
    }

    #[test]
    fn tokenize_args_handles_escaped_quote_inside_string() {
        // The escaped `\"` does not close the string; the whole `"x\"y"` is
        // one argument.
        assert_eq!(tokenize_args(r#""x\"y""#), vec![r#""x\"y""#.to_string()]);
    }

    #[test]
    fn tokenize_args_empty_returns_empty() {
        assert!(tokenize_args("").is_empty());
        assert!(tokenize_args("   \t\n  ").is_empty());
    }

    // ---------- join_statements ----------

    #[test]
    fn join_statements_one_per_logical_command() {
        let stmts = join_statements("set(A 1)\noption(B \"doc\" ON)\n");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].command, "set");
        assert_eq!(stmts[0].args, vec!["A".to_string(), "1".to_string()]);
        assert_eq!(stmts[1].command, "option");
        assert_eq!(
            stmts[1].args,
            vec!["B".to_string(), r#""doc""#.to_string(), "ON".to_string()]
        );
    }

    #[test]
    fn join_statements_joins_args_across_lines() {
        let stmts = join_statements("add_executable(\n  app\n  src/main.c\n  src/util.c\n)\n");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].command, "add_executable");
        assert_eq!(
            stmts[0].args,
            vec![
                "app".to_string(),
                "src/main.c".to_string(),
                "src/util.c".to_string(),
            ]
        );
    }

    #[test]
    fn join_statements_strips_comments_first() {
        let stmts = join_statements("# header\nset(X 1) # trailer\n");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].command, "set");
        assert_eq!(stmts[0].args, vec!["X".to_string(), "1".to_string()]);
    }

    #[test]
    fn join_statements_balances_nested_parens() {
        let stmts = join_statements("if(VAR AND (A OR B))\nset(X 1)\nendif()\n");
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0].command, "if");
        // The inner `(A OR B)` survives as its own tokens because tokenize_args
        // splits on whitespace and treats parens as ordinary characters.
        assert_eq!(
            stmts[0].args,
            vec![
                "VAR".to_string(),
                "AND".to_string(),
                "(A".to_string(),
                "OR".to_string(),
                "B)".to_string(),
            ]
        );
        assert_eq!(stmts[2].command, "endif");
    }

    #[test]
    fn join_statements_treats_parens_inside_strings_as_literal() {
        let stmts = join_statements(r#"set(X "a)b") set(Y 1)"#);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].args, vec!["X".to_string(), r#""a)b""#.to_string()]);
    }

    // ---------- unquote / arg_text ----------

    #[test]
    fn unquote_returns_inner_for_quoted() {
        assert_eq!(unquote(r#""hello""#), Some("hello".to_string()));
    }

    #[test]
    fn unquote_returns_none_for_unquoted() {
        assert_eq!(unquote("hello"), None);
    }

    #[test]
    fn unquote_handles_escapes() {
        // `\"` -> `"`, `\\` -> `\`, other `\x` -> `x`.
        assert_eq!(unquote(r#""a\"b""#), Some(r#"a"b"#.to_string()));
        assert_eq!(unquote(r#""a\\b""#), Some(r"a\b".to_string()));
        assert_eq!(unquote(r#""a\nb""#), Some("anb".to_string()));
    }

    #[test]
    fn unquote_rejects_too_short() {
        assert_eq!(unquote(""), None);
        assert_eq!(unquote(r#"""#), None);
    }

    #[test]
    fn arg_text_strips_quotes_when_present() {
        assert_eq!(arg_text(r#""hi""#), "hi");
        assert_eq!(arg_text("hi"), "hi");
    }

    // ---------- as_single_var_ref ----------

    #[test]
    fn as_single_var_ref_matches_bare_var_ref() {
        assert_eq!(as_single_var_ref("${VAR}"), Some("VAR"));
        assert_eq!(as_single_var_ref("  ${VAR}  "), Some("VAR"));
    }

    #[test]
    fn as_single_var_ref_rejects_concatenated() {
        assert_eq!(as_single_var_ref("${A}_${B}"), None);
        assert_eq!(as_single_var_ref("prefix_${VAR}"), None);
        assert_eq!(as_single_var_ref("${VAR}suffix"), None);
    }

    #[test]
    fn as_single_var_ref_rejects_invalid_ident() {
        assert_eq!(as_single_var_ref("${has space}"), None);
        assert_eq!(as_single_var_ref("${a-b}"), None);
        assert_eq!(as_single_var_ref("${}"), None);
    }

    // ---------- find_var_refs ----------

    #[test]
    fn find_var_refs_returns_each_occurrence_in_order() {
        let refs = find_var_refs("${A}_${B}_${A}");
        let names: Vec<&str> = refs.iter().map(|(_, _, n)| n.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "A"]);
    }

    #[test]
    fn find_var_refs_records_byte_offsets() {
        let s = "x${VAR}y";
        let refs = find_var_refs(s);
        assert_eq!(refs.len(), 1);
        let (start, end, name) = &refs[0];
        assert_eq!(name, "VAR");
        assert_eq!(&s[*start..*end], "${VAR}");
    }

    #[test]
    fn find_var_refs_ignores_malformed() {
        // No closing brace -> not a reference.
        assert!(find_var_refs("${VAR").is_empty());
        // Non-ident character inside braces -> not a reference.
        assert!(find_var_refs("${a-b}").is_empty());
        // `$` without `{` -> not a reference.
        assert!(find_var_refs("$VAR").is_empty());
    }

    // ---------- extract_if_gate ----------

    #[test]
    fn extract_if_gate_returns_known_var() {
        let args = vec!["BACKEND".to_string()];
        assert_eq!(
            extract_if_gate(&args, &known(&["BACKEND"])),
            Some("BACKEND".to_string()),
        );
    }

    #[test]
    fn extract_if_gate_returns_none_for_unknown() {
        let args = vec!["OTHER".to_string()];
        assert_eq!(extract_if_gate(&args, &known(&["BACKEND"])), None);
    }

    #[test]
    fn extract_if_gate_returns_none_for_empty() {
        assert_eq!(extract_if_gate(&[], &known(&["BACKEND"])), None);
    }

    #[test]
    fn extract_if_gate_strips_quotes() {
        // `if("BACKEND")` should still recognize the variable.
        let args = vec![r#""BACKEND""#.to_string()];
        assert_eq!(
            extract_if_gate(&args, &known(&["BACKEND"])),
            Some("BACKEND".to_string()),
        );
    }

    // ---------- substitute_var_template ----------

    #[test]
    fn substitute_var_template_replaces_known() {
        let out = substitute_var_template("${A}_${B}", &known(&["A", "B"]));
        assert_eq!(out, "{A}_{B}");
    }

    #[test]
    fn substitute_var_template_leaves_unknown_intact() {
        let out = substitute_var_template("${A}_${OTHER}", &known(&["A"]));
        assert_eq!(out, "{A}_${OTHER}");
    }

    #[test]
    fn substitute_var_template_preserves_surrounding_text() {
        let out = substitute_var_template("prefix-${A}-mid-${B}-tail", &known(&["A", "B"]));
        assert_eq!(out, "prefix-{A}-mid-{B}-tail");
    }

    // ---------- parse_configuration_json ----------

    #[test]
    fn parse_configuration_json_reads_valid_input() {
        let bytes = br#"{"configurable_variables": {"X": ["a","b"], "Y": [true,false]}}"#;
        let cfg = parse_configuration_json(bytes).expect("should parse");
        assert_eq!(cfg.variables.len(), 2);
        assert!(cfg.variables.contains_key("X"));
        assert!(cfg.variables.contains_key("Y"));
    }

    #[test]
    fn parse_configuration_json_empty_object_yields_no_variables() {
        let cfg = parse_configuration_json(b"{}").expect("should parse");
        assert!(cfg.variables.is_empty());
    }

    #[test]
    fn parse_configuration_json_returns_none_on_malformed() {
        assert!(parse_configuration_json(b"not json").is_none());
        assert!(parse_configuration_json(b"{\"configurable_variables\": []}").is_none());
    }

    // ---------- build_variables ----------

    fn raw_cfg(pairs: &[(&str, Vec<Value>)]) -> Configuration {
        let mut variables = BTreeMap::new();
        for (k, v) in pairs {
            variables.insert((*k).to_string(), v.clone());
        }
        Configuration { variables }
    }

    #[test]
    fn build_variables_classifies_booleans() {
        let cfg = raw_cfg(&[("FLAG", vec![Value::Bool(true), Value::Bool(false)])]);
        let vars = build_variables(&cfg, &HashMap::new());
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "FLAG");
        assert_eq!(vars[0].kind, ConfigVarKind::Boolean);
    }

    #[test]
    fn build_variables_classifies_string_enum() {
        let cfg = raw_cfg(&[(
            "MODE",
            vec![Value::String("fast".into()), Value::String("safe".into())],
        )]);
        let vars = build_variables(&cfg, &HashMap::new());
        assert_eq!(
            vars[0].kind,
            ConfigVarKind::Enum {
                values: vec!["fast".into(), "safe".into()],
                numeric: false,
            }
        );
    }

    #[test]
    fn build_variables_classifies_numeric_enum_from_numbers() {
        let cfg = raw_cfg(&[(
            "BITS",
            vec![
                Value::Number(serde_json::Number::from(32)),
                Value::Number(serde_json::Number::from(64)),
            ],
        )]);
        let vars = build_variables(&cfg, &HashMap::new());
        assert_eq!(
            vars[0].kind,
            ConfigVarKind::Enum {
                values: vec!["32".into(), "64".into()],
                numeric: true,
            }
        );
    }

    #[test]
    fn build_variables_classifies_numeric_enum_from_numeric_strings() {
        // Strings that parse as i64 keep `numeric: true`.
        let cfg = raw_cfg(&[(
            "BITS",
            vec![Value::String("32".into()), Value::String("64".into())],
        )]);
        let vars = build_variables(&cfg, &HashMap::new());
        assert_eq!(
            vars[0].kind,
            ConfigVarKind::Enum {
                values: vec!["32".into(), "64".into()],
                numeric: true,
            }
        );
    }

    #[test]
    fn build_variables_merges_cmake_default() {
        let cfg = raw_cfg(&[(
            "MODE",
            vec![Value::String("fast".into()), Value::String("safe".into())],
        )]);
        let mut defaults = HashMap::new();
        defaults.insert("MODE".to_string(), Some("safe".to_string()));
        let vars = build_variables(&cfg, &defaults);
        assert_eq!(vars[0].default.as_deref(), Some("safe"));
    }

    #[test]
    fn build_variables_default_absent_when_cmake_missing() {
        let cfg = raw_cfg(&[("MODE", vec![Value::String("fast".into())])]);
        let vars = build_variables(&cfg, &HashMap::new());
        assert_eq!(vars[0].default, None);
    }

    // ---------- handle_set ----------

    #[test]
    fn handle_set_records_cache_string_default() {
        let mut out = ScannedCmake::default();
        let mut sources = HashMap::new();
        let mut composed = HashMap::new();
        let args = vec![
            "MODE".to_string(),
            r#""fast""#.to_string(),
            "CACHE".to_string(),
            "STRING".to_string(),
            r#""doc""#.to_string(),
        ];
        handle_set(
            &args,
            &known(&["MODE"]),
            Path::new(""),
            &mut out,
            &mut sources,
            &mut composed,
        );
        assert_eq!(
            out.var_defaults.get("MODE"),
            Some(&Some("fast".to_string()))
        );
    }

    #[test]
    fn handle_set_ignores_cache_for_unknown_var() {
        let mut out = ScannedCmake::default();
        let mut sources = HashMap::new();
        let mut composed = HashMap::new();
        let args = vec![
            "OTHER".to_string(),
            r#""x""#.to_string(),
            "CACHE".to_string(),
            "STRING".to_string(),
            r#""doc""#.to_string(),
        ];
        handle_set(
            &args,
            &known(&["MODE"]),
            Path::new(""),
            &mut out,
            &mut sources,
            &mut composed,
        );
        assert!(out.var_defaults.is_empty());
    }

    #[test]
    fn handle_set_extracts_defines_from_cmake_c_flags() {
        let mut out = ScannedCmake::default();
        let mut sources = HashMap::new();
        let mut composed = HashMap::new();
        let args = vec![
            "CMAKE_C_FLAGS".to_string(),
            r#""${CMAKE_C_FLAGS} -DBITS=${WORD_SIZE}""#.to_string(),
        ];
        handle_set(
            &args,
            &known(&["WORD_SIZE"]),
            Path::new(""),
            &mut out,
            &mut sources,
            &mut composed,
        );
        assert_eq!(out.defines.len(), 1);
        assert_eq!(out.defines[0].c_name, "BITS");
        assert_eq!(
            out.defines[0].kind,
            DefineKind::Bare {
                var: "WORD_SIZE".to_string()
            }
        );
    }

    #[test]
    fn handle_set_captures_source_list() {
        let mut out = ScannedCmake::default();
        let mut sources = HashMap::new();
        let mut composed = HashMap::new();
        let args = vec![
            "BACKEND_SOURCES".to_string(),
            "src/backend_${BACKEND}.c".to_string(),
        ];
        handle_set(
            &args,
            &known(&["BACKEND"]),
            Path::new(""),
            &mut out,
            &mut sources,
            &mut composed,
        );
        assert_eq!(
            sources.get("BACKEND_SOURCES"),
            Some(&vec!["src/backend_${BACKEND}.c".to_string()])
        );
    }

    #[test]
    fn handle_set_remembers_composed_value() {
        let mut out = ScannedCmake::default();
        let mut sources = HashMap::new();
        let mut composed = HashMap::new();
        let args = vec![
            "BUILD_PROFILE".to_string(),
            r#""${BACKEND}_${WORD_SIZE}""#.to_string(),
        ];
        handle_set(
            &args,
            &known(&["BACKEND", "WORD_SIZE"]),
            Path::new(""),
            &mut out,
            &mut sources,
            &mut composed,
        );
        assert_eq!(
            composed.get("BUILD_PROFILE"),
            Some(&"${BACKEND}_${WORD_SIZE}".to_string())
        );
    }

    // ---------- handle_option ----------

    #[test]
    fn handle_option_maps_on_off_to_bool_strings() {
        for (literal, expected) in [
            ("ON", "true"),
            ("TRUE", "true"),
            ("YES", "true"),
            ("1", "true"),
            ("OFF", "false"),
            ("FALSE", "false"),
            ("NO", "false"),
            ("0", "false"),
        ] {
            let mut out = ScannedCmake::default();
            let args = vec![
                "FLAG".to_string(),
                r#""doc""#.to_string(),
                literal.to_string(),
            ];
            handle_option(&args, &known(&["FLAG"]), &mut out);
            assert_eq!(
                out.var_defaults.get("FLAG"),
                Some(&Some(expected.to_string())),
                "for {literal}"
            );
        }
    }

    #[test]
    fn handle_option_records_no_default_when_missing() {
        let mut out = ScannedCmake::default();
        let args = vec!["FLAG".to_string(), r#""doc""#.to_string()];
        handle_option(&args, &known(&["FLAG"]), &mut out);
        assert_eq!(out.var_defaults.get("FLAG"), Some(&None));
    }

    #[test]
    fn handle_option_ignores_unknown_var() {
        let mut out = ScannedCmake::default();
        let args = vec![
            "OTHER".to_string(),
            r#""doc""#.to_string(),
            "ON".to_string(),
        ];
        handle_option(&args, &known(&["FLAG"]), &mut out);
        assert!(out.var_defaults.is_empty());
    }

    // ---------- handle_add_compile_definitions ----------

    #[test]
    fn handle_add_compile_definitions_recognizes_quoted_string() {
        let mut out = ScannedCmake::default();
        let args = vec![r#""APP_MODE_STR=\"${APP_MODE}\"""#.to_string()];
        handle_add_compile_definitions(&args, &known(&["APP_MODE"]), &mut out);
        assert_eq!(out.defines.len(), 1);
        assert_eq!(out.defines[0].c_name, "APP_MODE_STR");
        assert_eq!(
            out.defines[0].kind,
            DefineKind::QuotedString {
                var: "APP_MODE".to_string()
            }
        );
    }

    #[test]
    fn handle_add_compile_definitions_recognizes_bare() {
        let mut out = ScannedCmake::default();
        let args = vec![r#""WORD_SIZE=${WORD_SIZE}""#.to_string()];
        handle_add_compile_definitions(&args, &known(&["WORD_SIZE"]), &mut out);
        assert_eq!(out.defines.len(), 1);
        assert_eq!(
            out.defines[0].kind,
            DefineKind::Bare {
                var: "WORD_SIZE".to_string()
            }
        );
    }

    #[test]
    fn handle_add_compile_definitions_recognizes_composed() {
        let mut out = ScannedCmake::default();
        let args = vec![r#""PROFILE=${BACKEND}_${WORD_SIZE}""#.to_string()];
        handle_add_compile_definitions(&args, &known(&["BACKEND", "WORD_SIZE"]), &mut out);
        assert_eq!(out.defines.len(), 1);
        assert_eq!(
            out.defines[0].kind,
            DefineKind::Composed {
                template: "{BACKEND}_{WORD_SIZE}".to_string()
            }
        );
        assert_eq!(out.defines[0].source_vars, vec!["BACKEND", "WORD_SIZE"]);
    }

    #[test]
    fn handle_add_compile_definitions_skips_unknown_var() {
        let mut out = ScannedCmake::default();
        let args = vec![r#""X=${UNKNOWN}""#.to_string()];
        handle_add_compile_definitions(&args, &known(&["BACKEND"]), &mut out);
        assert!(out.defines.is_empty());
    }

    // ---------- handle_add_definitions ----------

    #[test]
    fn handle_add_definitions_emits_gated_flag_inside_if() {
        let mut out = ScannedCmake::default();
        let args = vec!["-DENABLE_EXTRA".to_string()];
        handle_add_definitions(
            &args,
            &known(&["ENABLE_EXTRA"]),
            &mut out,
            Some("ENABLE_EXTRA"),
        );
        assert_eq!(out.defines.len(), 1);
        assert_eq!(out.defines[0].c_name, "ENABLE_EXTRA");
        assert_eq!(
            out.defines[0].kind,
            DefineKind::GatedFlag {
                gate_var: "ENABLE_EXTRA".to_string()
            }
        );
    }

    #[test]
    fn handle_add_definitions_skips_bare_flag_outside_if() {
        let mut out = ScannedCmake::default();
        let args = vec!["-DENABLE_EXTRA".to_string()];
        handle_add_definitions(&args, &known(&["ENABLE_EXTRA"]), &mut out, None);
        assert!(out.defines.is_empty());
    }

    #[test]
    fn handle_add_definitions_emits_bare_for_known_value() {
        let mut out = ScannedCmake::default();
        let args = vec!["-DWORD_SIZE=${WORD_SIZE}".to_string()];
        handle_add_definitions(&args, &known(&["WORD_SIZE"]), &mut out, None);
        assert_eq!(out.defines.len(), 1);
        assert_eq!(
            out.defines[0].kind,
            DefineKind::Bare {
                var: "WORD_SIZE".to_string()
            }
        );
    }

    // ---------- extract_defines_from_flags ----------

    #[test]
    fn extract_defines_from_flags_picks_bare_known() {
        let mut defines = Vec::new();
        extract_defines_from_flags(
            "${CMAKE_C_FLAGS} -DBITS=${WORD_SIZE} -O2",
            &known(&["WORD_SIZE"]),
            &HashMap::new(),
            &mut defines,
        );
        assert_eq!(defines.len(), 1);
        assert_eq!(defines[0].c_name, "BITS");
        assert_eq!(
            defines[0].kind,
            DefineKind::Bare {
                var: "WORD_SIZE".to_string()
            }
        );
    }

    #[test]
    fn extract_defines_from_flags_picks_composed_inline() {
        let mut defines = Vec::new();
        extract_defines_from_flags(
            "-DPROFILE=${BACKEND}_${WORD_SIZE}",
            &known(&["BACKEND", "WORD_SIZE"]),
            &HashMap::new(),
            &mut defines,
        );
        assert_eq!(defines.len(), 1);
        assert_eq!(
            defines[0].kind,
            DefineKind::Composed {
                template: "{BACKEND}_{WORD_SIZE}".to_string()
            }
        );
    }

    #[test]
    fn extract_defines_from_flags_resolves_via_composed_var() {
        // `-DPROFILE=${BUILD_PROFILE}` where BUILD_PROFILE was previously
        // defined as `${BACKEND}_${WORD_SIZE}` -> Composed.
        let mut composed = HashMap::new();
        composed.insert(
            "BUILD_PROFILE".to_string(),
            "${BACKEND}_${WORD_SIZE}".to_string(),
        );
        let mut defines = Vec::new();
        extract_defines_from_flags(
            "-DPROFILE=${BUILD_PROFILE}",
            &known(&["BACKEND", "WORD_SIZE"]),
            &composed,
            &mut defines,
        );
        assert_eq!(defines.len(), 1);
        assert_eq!(
            defines[0].kind,
            DefineKind::Composed {
                template: "{BACKEND}_{WORD_SIZE}".to_string()
            }
        );
    }

    #[test]
    fn extract_defines_from_flags_skips_non_d_tokens() {
        let mut defines = Vec::new();
        extract_defines_from_flags(
            "-O2 -Wall -fPIC",
            &known(&["WORD_SIZE"]),
            &HashMap::new(),
            &mut defines,
        );
        assert!(defines.is_empty());
    }

    // ---------- enumerate_variants ----------

    #[test]
    fn enumerate_variants_groups_files_by_value() {
        let files: HashSet<PathBuf> = ["src/backend_alpha.c", "src/backend_beta.c", "src/main.c"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let variants = enumerate_variants("src/backend_${BACKEND}.c", "BACKEND", &files);
        assert_eq!(variants.len(), 2);
        // BTreeSet ordering -> alphabetical.
        assert_eq!(variants[0].value, "alpha");
        assert_eq!(
            variants[0].files,
            vec![PathBuf::from("src/backend_alpha.c")]
        );
        assert_eq!(variants[1].value, "beta");
    }

    #[test]
    fn enumerate_variants_returns_empty_when_pattern_missing_ref() {
        let files: HashSet<PathBuf> = [PathBuf::from("src/main.c")].into_iter().collect();
        let variants = enumerate_variants("src/main.c", "BACKEND", &files);
        assert!(variants.is_empty());
    }

    #[test]
    fn enumerate_variants_rejects_non_ident_values() {
        // Files that would match with a non-identifier value (e.g. dashes) are
        // skipped.
        let files: HashSet<PathBuf> = [
            PathBuf::from("src/backend_a-b.c"),
            PathBuf::from("src/backend_ok.c"),
        ]
        .into_iter()
        .collect();
        let variants = enumerate_variants("src/backend_${BACKEND}.c", "BACKEND", &files);
        let values: Vec<&str> = variants.iter().map(|v| v.value.as_str()).collect();
        assert_eq!(values, vec!["ok"]);
    }

    // ---------- handle_target_compile_definitions ----------

    #[test]
    fn handle_target_compile_definitions_emits_gated_flag() {
        let mut out = ScannedCmake::default();
        let args = vec![
            "app".to_string(),
            "PRIVATE".to_string(),
            "ENABLE_EXTRA".to_string(),
        ];
        handle_target_compile_definitions(&args, Some("ENABLE_EXTRA"), &mut out);
        assert_eq!(out.defines.len(), 1);
        assert_eq!(out.defines[0].c_name, "ENABLE_EXTRA");
        assert_eq!(
            out.defines[0].kind,
            DefineKind::GatedFlag {
                gate_var: "ENABLE_EXTRA".to_string()
            }
        );
    }

    #[test]
    fn handle_target_compile_definitions_no_op_outside_gate() {
        let mut out = ScannedCmake::default();
        let args = vec!["app".to_string(), "PRIVATE".to_string(), "FLAG".to_string()];
        handle_target_compile_definitions(&args, None, &mut out);
        assert!(out.defines.is_empty());
    }

    #[test]
    fn handle_target_compile_definitions_dedupes_same_gate() {
        let mut out = ScannedCmake::default();
        let args = vec!["app".to_string(), "PRIVATE".to_string(), "FLAG".to_string()];
        handle_target_compile_definitions(&args, Some("ENABLE_EXTRA"), &mut out);
        // Second call with the same gate should not add a duplicate.
        handle_target_compile_definitions(&args, Some("ENABLE_EXTRA"), &mut out);
        assert_eq!(out.defines.len(), 1);
    }

    // ---------- rewrite_composed_defines ----------

    #[test]
    fn rewrite_composed_defines_promotes_bare_to_composed() {
        let mut defines = vec![DefineMapping {
            c_name: "PROFILE".to_string(),
            kind: DefineKind::Bare {
                var: "BUILD_PROFILE".to_string(),
            },
            source_vars: vec!["BUILD_PROFILE".to_string()],
        }];
        let mut composed = HashMap::new();
        composed.insert(
            "BUILD_PROFILE".to_string(),
            "${BACKEND}_${WORD_SIZE}".to_string(),
        );
        rewrite_composed_defines(&mut defines, &composed, &known(&["BACKEND", "WORD_SIZE"]));
        assert_eq!(
            defines[0].kind,
            DefineKind::Composed {
                template: "{BACKEND}_{WORD_SIZE}".to_string()
            }
        );
        assert_eq!(defines[0].source_vars, vec!["BACKEND", "WORD_SIZE"]);
    }

    #[test]
    fn rewrite_composed_defines_leaves_unrelated_bare_alone() {
        let mut defines = vec![DefineMapping {
            c_name: "BITS".to_string(),
            kind: DefineKind::Bare {
                var: "WORD_SIZE".to_string(),
            },
            source_vars: vec!["WORD_SIZE".to_string()],
        }];
        // WORD_SIZE is NOT in composed_vars, so it stays Bare.
        rewrite_composed_defines(&mut defines, &HashMap::new(), &known(&["WORD_SIZE"]));
        assert_eq!(
            defines[0].kind,
            DefineKind::Bare {
                var: "WORD_SIZE".to_string()
            }
        );
    }

    // ---------- normalize_join / prefix_source_path ----------

    #[test]
    fn normalize_join_appends_descendant() {
        assert_eq!(
            normalize_join(Path::new("lib"), Path::new("blake")).as_deref(),
            Some(Path::new("lib/blake")),
        );
        assert_eq!(
            normalize_join(Path::new(""), Path::new("app")).as_deref(),
            Some(Path::new("app")),
        );
    }

    #[test]
    fn normalize_join_resolves_parent_dir() {
        assert_eq!(
            normalize_join(Path::new("lib/blake"), Path::new("../../app/src/utils.c")).as_deref(),
            Some(Path::new("app/src/utils.c")),
        );
    }

    #[test]
    fn normalize_join_returns_none_when_escapes_root() {
        assert_eq!(
            normalize_join(Path::new("lib"), Path::new("../../oops")),
            None
        );
    }

    #[test]
    fn normalize_join_drops_curdir_segments() {
        assert_eq!(
            normalize_join(Path::new("lib"), Path::new("./src/./foo.c")).as_deref(),
            Some(Path::new("lib/src/foo.c")),
        );
    }

    #[test]
    fn prefix_source_path_passes_through_at_root() {
        assert_eq!(
            prefix_source_path(Path::new(""), "src/main.c"),
            "src/main.c"
        );
    }

    #[test]
    fn prefix_source_path_prepends_rel_dir() {
        assert_eq!(
            prefix_source_path(Path::new("lib"), "src/backend.c"),
            "lib/src/backend.c"
        );
    }

    #[test]
    fn prefix_source_path_preserves_var_placeholders() {
        // `${BACKEND}` is not normalized -- the literal placeholder survives so
        // `enumerate_variants` can match against the project file set.
        assert_eq!(
            prefix_source_path(Path::new("lib"), "src/backend_${BACKEND}.c"),
            "lib/src/backend_${BACKEND}.c"
        );
    }

    // ---------- handle_set with rel_dir ----------

    #[test]
    fn handle_set_prefixes_sources_with_rel_dir() {
        let mut out = ScannedCmake::default();
        let mut sources = HashMap::new();
        let mut composed = HashMap::new();
        let args = vec![
            "BLAKE_SOURCES".to_string(),
            "src/blake256.c".to_string(),
            "src/thash_blake_${THASH}.c".to_string(),
            "../../app/src/utils.c".to_string(),
        ];
        handle_set(
            &args,
            &known(&["THASH"]),
            Path::new("lib/blake"),
            &mut out,
            &mut sources,
            &mut composed,
        );
        let got = sources.get("BLAKE_SOURCES").unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], "lib/blake/src/blake256.c");
        assert_eq!(got[1], "lib/blake/src/thash_blake_${THASH}.c");
        assert_eq!(got[2], "app/src/utils.c");
    }

    // ---------- add_subdirectory ----------
    //
    // These exercise `scan` end-to-end against an in-memory `RawDir` so the
    // recursive `add_subdirectory` path is covered, including the literal /
    // variable / missing-CMakeLists / escaping-root branches.

    fn build_dir(files: &[(&str, &str)]) -> RawDir {
        let mut dir = RawDir::default();
        for (path, contents) in files {
            dir.set_file(path, contents.as_bytes().to_vec())
                .unwrap_or_else(|e| panic!("set_file({path}) failed: {e:?}"));
        }
        dir
    }

    #[test]
    fn add_subdirectory_descends_into_literal_path() {
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"MODE": ["fast", "safe"]}}"#,
            ),
            (
                "CMakeLists.txt",
                "set(MODE \"fast\" CACHE STRING \"doc\")\nadd_subdirectory(lib)\n",
            ),
            (
                "lib/CMakeLists.txt",
                "add_compile_definitions(\"MODE_STR=\\\"${MODE}\\\"\")\nadd_library(core src/lib.c)\n",
            ),
            ("lib/src/lib.c", "// stub\n"),
        ]);
        let ir = scan(&dir);
        assert!(!ir.is_empty);
        // The compile-definition declared inside `lib/CMakeLists.txt` made it
        // into the IR.
        assert_eq!(ir.defines.len(), 1);
        assert_eq!(ir.defines[0].c_name, "MODE_STR");
        assert_eq!(
            ir.defines[0].kind,
            DefineKind::QuotedString {
                var: "MODE".to_string()
            }
        );
    }

    #[test]
    fn add_subdirectory_variable_form_emits_one_variant_per_value() {
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"BACKEND": ["alpha", "beta"], "MODE": ["fast", "safe"]}}"#,
            ),
            (
                "CMakeLists.txt",
                "set(BACKEND \"alpha\" CACHE STRING \"doc\")\n\
                 set(MODE \"fast\" CACHE STRING \"doc\")\n\
                 add_subdirectory(${BACKEND})\n",
            ),
            (
                "alpha/CMakeLists.txt",
                "add_compile_definitions(\"PICKED_MODE=${MODE}\")\n",
            ),
            (
                "beta/CMakeLists.txt",
                "add_compile_definitions(\"PICKED_MODE=${MODE}\")\n",
            ),
        ]);
        let ir = scan(&dir);
        // The variant subtrees do NOT leak into the top-level defines.
        assert!(ir.defines.is_empty(), "ir.defines = {:?}", ir.defines);
        assert_eq!(ir.subdir_selections.len(), 1);
        let sel = &ir.subdir_selections[0];
        assert_eq!(sel.driving_var, "BACKEND");
        assert_eq!(sel.variants.len(), 2);
        let alpha = sel.variants.iter().find(|v| v.value == "alpha").unwrap();
        assert_eq!(alpha.path, PathBuf::from("alpha"));
        assert_eq!(alpha.defines.len(), 1);
        assert_eq!(alpha.defines[0].c_name, "PICKED_MODE");
        assert_eq!(
            alpha.defines[0].kind,
            DefineKind::Bare {
                var: "MODE".to_string()
            }
        );
        let beta = sel.variants.iter().find(|v| v.value == "beta").unwrap();
        assert_eq!(beta.path, PathBuf::from("beta"));
        assert_eq!(beta.defines.len(), 1);
        assert_eq!(beta.defines[0].c_name, "PICKED_MODE");
    }

    #[test]
    fn add_subdirectory_variable_form_skips_missing_variant() {
        // `BACKEND` declares two values but only `alpha/` exists on disk.
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"BACKEND": ["alpha", "beta"], "MODE": ["fast"]}}"#,
            ),
            ("CMakeLists.txt", "add_subdirectory(${BACKEND})\n"),
            (
                "alpha/CMakeLists.txt",
                "add_compile_definitions(\"TAG=${MODE}\")\n",
            ),
        ]);
        let ir = scan(&dir);
        assert_eq!(ir.subdir_selections.len(), 1);
        let sel = &ir.subdir_selections[0];
        assert_eq!(sel.variants.len(), 1);
        assert_eq!(sel.variants[0].value, "alpha");
    }

    #[test]
    fn add_subdirectory_variable_form_isolates_variants() {
        // The `CMAKE_C_FLAGS` accumulator must NOT leak `alpha`'s `-DTAG=alpha`
        // into the `beta` variant (and vice versa). Each variant's defines
        // come only from its own subtree.
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"BACKEND": ["alpha", "beta"]}}"#,
            ),
            ("CMakeLists.txt", "add_subdirectory(${BACKEND})\n"),
            (
                "alpha/CMakeLists.txt",
                "set(CMAKE_C_FLAGS \"${CMAKE_C_FLAGS} -DTAG=${BACKEND}\")\n",
            ),
            (
                "beta/CMakeLists.txt",
                "set(CMAKE_C_FLAGS \"${CMAKE_C_FLAGS} -DTAG=${BACKEND}\")\n",
            ),
        ]);
        let ir = scan(&dir);
        let sel = &ir.subdir_selections[0];
        for variant in &sel.variants {
            assert_eq!(
                variant.defines.len(),
                1,
                "variant `{}` should record exactly one define (got {:?})",
                variant.value,
                variant.defines,
            );
            assert_eq!(variant.defines[0].c_name, "TAG");
        }
    }

    #[test]
    fn add_subdirectory_variable_form_nests() {
        // A `add_subdirectory(${OUTER})` selection can itself contain a
        // `add_subdirectory(${INNER})` selection inside one of its variants.
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"OUTER": ["x", "y"], "INNER": ["one", "two"], "MODE": ["fast"]}}"#,
            ),
            ("CMakeLists.txt", "add_subdirectory(${OUTER})\n"),
            ("x/CMakeLists.txt", "add_subdirectory(${INNER})\n"),
            (
                "x/one/CMakeLists.txt",
                "add_compile_definitions(\"DEEP=${MODE}\")\n",
            ),
            (
                "x/two/CMakeLists.txt",
                "add_compile_definitions(\"DEEP=${MODE}\")\n",
            ),
            (
                "y/CMakeLists.txt",
                "add_compile_definitions(\"SHALLOW=${MODE}\")\n",
            ),
        ]);
        let ir = scan(&dir);
        assert_eq!(ir.subdir_selections.len(), 1);
        let outer = &ir.subdir_selections[0];
        assert_eq!(outer.driving_var, "OUTER");

        let x = outer.variants.iter().find(|v| v.value == "x").unwrap();
        assert!(x.defines.is_empty(), "x.defines = {:?}", x.defines);
        assert_eq!(x.subdir_selections.len(), 1);
        let inner = &x.subdir_selections[0];
        assert_eq!(inner.driving_var, "INNER");
        assert_eq!(inner.variants.len(), 2);

        let y = outer.variants.iter().find(|v| v.value == "y").unwrap();
        assert_eq!(y.defines.len(), 1);
        assert_eq!(y.defines[0].c_name, "SHALLOW");
        assert!(y.subdir_selections.is_empty());
    }

    #[test]
    fn add_subdirectory_variable_form_resolves_target_inside_variant() {
        // Mirrors sphincs's `lib/${HASH_BACKEND}/CMakeLists.txt` pattern, where
        // each variant subdir declares its own target and sources via the
        // `*_SOURCES` indirection.
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"HASH_BACKEND": ["blake", "haraka"]}}"#,
            ),
            (
                "CMakeLists.txt",
                "set(HASH_BACKEND \"blake\" CACHE STRING \"doc\")\nadd_subdirectory(lib)\n",
            ),
            ("lib/CMakeLists.txt", "add_subdirectory(${HASH_BACKEND})\n"),
            (
                "lib/blake/CMakeLists.txt",
                "set(BLAKE_SOURCES src/blake256.c)\n\
                 add_library(blake SHARED ${BLAKE_SOURCES})\n",
            ),
            (
                "lib/haraka/CMakeLists.txt",
                "set(HARAKA_SOURCES src/haraka.c)\n\
                 add_library(haraka SHARED ${HARAKA_SOURCES})\n",
            ),
            ("lib/blake/src/blake256.c", "// stub\n"),
            ("lib/haraka/src/haraka.c", "// stub\n"),
        ]);
        let ir = scan(&dir);
        assert!(ir.source_selections.is_empty());
        assert_eq!(ir.subdir_selections.len(), 1);
        let sel = &ir.subdir_selections[0];
        assert_eq!(sel.driving_var, "HASH_BACKEND");
        let blake = sel.variants.iter().find(|v| v.value == "blake").unwrap();
        assert_eq!(blake.path, PathBuf::from("lib/blake"));
        // Source paths are normalized project-relative.
        assert!(
            blake.source_selections.is_empty(),
            "no `${{VAR}}` in sources, so no SourceSelection inside the variant; \
             blake.source_selections = {:?}",
            blake.source_selections,
        );
        let haraka = sel.variants.iter().find(|v| v.value == "haraka").unwrap();
        assert_eq!(haraka.path, PathBuf::from("lib/haraka"));
    }

    #[test]
    fn add_subdirectory_with_missing_cmakelists_is_warn_and_continue() {
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"MODE": ["fast"]}}"#,
            ),
            (
                "CMakeLists.txt",
                "add_subdirectory(missing)\nadd_compile_definitions(\"AFTER=${MODE}\")\n",
            ),
        ]);
        let ir = scan(&dir);
        // Statements AFTER the failed add_subdirectory still scan.
        assert_eq!(ir.defines.len(), 1);
        assert_eq!(ir.defines[0].c_name, "AFTER");
    }

    #[test]
    fn add_subdirectory_nested_recursion_works() {
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"MODE": ["fast"]}}"#,
            ),
            ("CMakeLists.txt", "add_subdirectory(a)\n"),
            ("a/CMakeLists.txt", "add_subdirectory(b)\n"),
            (
                "a/b/CMakeLists.txt",
                "add_compile_definitions(\"NESTED=${MODE}\")\n",
            ),
        ]);
        let ir = scan(&dir);
        assert_eq!(ir.defines.len(), 1);
        assert_eq!(ir.defines[0].c_name, "NESTED");
    }

    #[test]
    fn add_subdirectory_source_paths_resolve_through_rel_dir() {
        // The library declares its sources in a `*_SOURCES` variable inside the
        // subdirectory (the conventional CMake idiom). The IR must store those
        // files as project-relative paths.
        let dir = build_dir(&[
            (
                "configuration.json",
                r#"{"configurable_variables": {"BACKEND": ["alpha", "beta"]}}"#,
            ),
            (
                "CMakeLists.txt",
                "set(BACKEND \"alpha\" CACHE STRING \"doc\")\nadd_subdirectory(lib)\n",
            ),
            (
                "lib/CMakeLists.txt",
                "set(CORE_SOURCES src/backend_${BACKEND}.c)\n\
                 add_library(core ${CORE_SOURCES})\n",
            ),
            ("lib/src/backend_alpha.c", "// stub\n"),
            ("lib/src/backend_beta.c", "// stub\n"),
        ]);
        let ir = scan(&dir);
        assert_eq!(ir.source_selections.len(), 1, "ir = {ir}");
        let sel = &ir.source_selections[0];
        assert_eq!(sel.target, "core");
        assert_eq!(sel.driving_var, "BACKEND");
        assert_eq!(sel.variants.len(), 2);
        let alpha = sel.variants.iter().find(|v| v.value == "alpha").unwrap();
        assert_eq!(alpha.files, vec![PathBuf::from("lib/src/backend_alpha.c")]);
        let beta = sel.variants.iter().find(|v| v.value == "beta").unwrap();
        assert_eq!(beta.files, vec![PathBuf::from("lib/src/backend_beta.c")]);
    }
}
