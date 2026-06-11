use harvest_core::config::AgentKind;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitStatus};
use tracing::{info, warn};

#[derive(Debug, Clone, Copy)]
pub enum AgentPhase {
    Translate,
    Verify,
}

impl AgentPhase {
    fn label(self) -> &'static str {
        match self {
            AgentPhase::Translate => "translation",
            AgentPhase::Verify => "verification",
        }
    }

    fn log_file_name(self) -> &'static str {
        match self {
            AgentPhase::Translate => "translation.log",
            AgentPhase::Verify => "verify.log",
        }
    }

    fn opencode_agent_name(self) -> &'static str {
        match self {
            AgentPhase::Translate => "harvest-translate",
            AgentPhase::Verify => "harvest-verify",
        }
    }

    fn opencode_description(self) -> &'static str {
        match self {
            AgentPhase::Translate => "Harvest agentic translation backend",
            AgentPhase::Verify => "Harvest agentic verification backend",
        }
    }

    fn append_system_prompt(self) -> &'static str {
        match self {
            AgentPhase::Translate => "After any context compaction, you MUST first read PLAN.md.",
            AgentPhase::Verify => {
                "After any context compaction, you MUST first read PLAN.md and HYPOTHESES.md."
            }
        }
    }
}

pub struct AgentInvocation<'a> {
    pub phase: AgentPhase,
    pub agent: AgentKind,
    pub work_dir: &'a Path,
    pub prompt: &'a str,
    pub timeout_secs: u64,
    pub model: Option<&'a str>,
    pub no_plan: bool,
    pub extra_env: &'a HashMap<String, String>,
    pub output_log_path: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeModelLimits {
    pub context: u64,
    pub output: Option<u64>,
}

pub fn load_opencode_model_limits(
    model: &str,
) -> Result<OpenCodeModelLimits, Box<dyn std::error::Error>> {
    let (provider, id) = parse_opencode_model(model)?;
    let provider_output = run_opencode_models(Some(&provider))?;
    if let Some(limits) = extract_model_limits_from_output(&provider_output, &provider, &id) {
        info!(
            "Resolved OpenCode model limits from provider listing (provider={provider}, id={id}): context={}, output={:?}",
            limits.context, limits.output,
        );
        return Ok(limits);
    }

    let all_output = run_opencode_models(None)?;
    if let Some(limits) = extract_model_limits_from_output(&all_output, &provider, &id) {
        info!(
            "Resolved OpenCode model limits from global listing (provider={provider}, id={id}): context={}, output={:?}",
            limits.context, limits.output,
        );
        return Ok(limits);
    }

    Err(format!(
        "OpenCode model metadata not found for {model}; run `opencode models --verbose` and verify the model exists with a limit.context field"
    ).into())
}

pub fn render_model_limits_block(limits: &OpenCodeModelLimits) -> String {
    let mut lines = vec![
        "### Registry context limits".to_string(),
        format!("context_limit: {}", limits.context),
    ];
    if let Some(output) = limits.output {
        lines.push(format!("output_limit: {output}"));
    }
    lines.join("\n")
}

fn parse_opencode_model(model: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    match model.split_once('/') {
        Some((provider, id)) if !provider.is_empty() && !id.is_empty() => {
            Ok((provider.to_string(), id.to_string()))
        }
        _ => Err(format!("OpenCode model must be in provider/model format, got: {model}").into()),
    }
}

fn run_opencode_models(provider: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let mut cmd = Command::new("opencode");
    cmd.arg("models");
    if let Some(provider) = provider {
        cmd.arg(provider);
    }
    cmd.arg("--verbose");
    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "opencode models --verbose failed (status={}): {}",
            output.status,
            stderr.trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn extract_model_limits_from_output(
    output: &str,
    expected_provider: &str,
    expected_id: &str,
) -> Option<OpenCodeModelLimits> {
    let mut buf: Vec<String> = Vec::new();
    let mut collecting = false;
    let mut brace_depth: i32 = 0;

    for raw_line in output.lines() {
        let trimmed = raw_line.trim();
        if !collecting {
            if trimmed.starts_with('{') {
                buf.clear();
                buf.push(trimmed.to_string());
                collecting = true;
                brace_depth = trimmed.chars().filter(|&c| c == '{').count() as i32
                    - trimmed.chars().filter(|&c| c == '}').count() as i32;
                if brace_depth <= 0 {
                    // Single-line JSON object
                    collecting = false;
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        let provider = value
                            .get("providerID")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let id = value.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if provider == expected_provider && id == expected_id {
                            let Some(limit) = value.get("limit") else {
                                continue;
                            };
                            let Some(context) = limit.get("context").and_then(|v| v.as_u64())
                            else {
                                continue;
                            };
                            let output = limit.get("output").and_then(|v| v.as_u64());
                            return Some(OpenCodeModelLimits { context, output });
                        }
                    }
                }
            }
            continue;
        }

        buf.push(trimmed.to_string());
        brace_depth += trimmed.chars().filter(|&c| c == '{').count() as i32
            - trimmed.chars().filter(|&c| c == '}').count() as i32;
        if brace_depth > 0 {
            continue;
        }

        let joined = buf.join("\n");
        collecting = false;
        buf.clear();

        let value = match serde_json::from_str::<serde_json::Value>(&joined) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let provider = value
            .get("providerID")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let id = value.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if provider == expected_provider && id == expected_id {
            let Some(limit) = value.get("limit") else {
                continue;
            };
            let Some(context) = limit.get("context").and_then(|v| v.as_u64()) else {
                continue;
            };
            let output = limit.get("output").and_then(|v| v.as_u64());
            return Some(OpenCodeModelLimits { context, output });
        }
    }

    None
}

pub fn invoke_agent(invocation: AgentInvocation<'_>) -> Result<(), Box<dyn std::error::Error>> {
    prepare_agent_files(&invocation)?;

    let logs_dir = invocation
        .work_dir
        .parent()
        .unwrap_or(invocation.work_dir)
        .join("logs");
    fs::create_dir_all(&logs_dir)?;
    let log_path = logs_dir.join(invocation.phase.log_file_name());

    let status = match invocation.agent {
        AgentKind::Kiro => invoke_kiro(&invocation, &log_path)?,
        AgentKind::Claude => invoke_claude(&invocation, &log_path)?,
        AgentKind::OpenCode => invoke_opencode(&invocation, &log_path)?,
    };

    if !status.success() {
        warn!("{} agent exited with {status}", invocation.phase.label());
    }

    if invocation.agent == AgentKind::OpenCode {
        if let Err(e) = export_opencode_sessions(&log_path, invocation.output_log_path) {
            warn!("OpenCode session export failed (non-fatal): {e}");
        }
    }

    append_trace_if_requested(&log_path, invocation.output_log_path)?;
    Ok(())
}

fn prepare_agent_files(invocation: &AgentInvocation<'_>) -> Result<(), Box<dyn std::error::Error>> {
    match invocation.agent {
        AgentKind::Kiro => Ok(()),
        AgentKind::Claude => {
            let case_dir = invocation.work_dir.parent().unwrap_or(invocation.work_dir);
            write_claude_sandbox(case_dir)?;
            Ok(())
        }
        AgentKind::OpenCode => write_opencode_agent(
            invocation.work_dir,
            OpenCodeAgentConfig {
                name: invocation.phase.opencode_agent_name(),
                description: invocation.phase.opencode_description(),
                system_prompt: invocation.phase.append_system_prompt(),
            },
        ),
    }
}

fn invoke_kiro(
    invocation: &AgentInvocation<'_>,
    log_path: &Path,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    info!(
        "Invoking Kiro {} agent (timeout={}s, extra_env={} vars)",
        invocation.phase.label(),
        invocation.timeout_secs,
        invocation.extra_env.len()
    );
    run_bash_agent(
        invocation,
        log_path,
        format!(
            "set -o pipefail; timeout {} kiro-cli chat \
             --no-interactive --trust-all-tools \"$PROMPT\" < /dev/null 2>&1 | tee \"$LOG\"",
            invocation.timeout_secs
        ),
        None,
    )
}

fn invoke_claude(
    invocation: &AgentInvocation<'_>,
    log_path: &Path,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let use_ccr = claude_uses_ccr(invocation.model);
    info!(
        "Invoking Claude Code {} agent (model={}, no_plan={}, timeout={}s, ccr={}, extra_env={} vars)",
        invocation.phase.label(),
        invocation.model.unwrap_or("(cli default)"),
        invocation.no_plan,
        invocation.timeout_secs,
        use_ccr,
        invocation.extra_env.len()
    );

    let model_flag = invocation
        .model
        .map(|_| "--model \"$MODEL\" ")
        .unwrap_or_default();
    let append_sys_flag = if invocation.no_plan {
        ""
    } else {
        "--append-system-prompt \"$APPEND_SYS\" "
    };

    let status = run_bash_agent(
        invocation,
        log_path,
        format!(
            "set -o pipefail; timeout {} claude -p \"$PROMPT\" \
             {model_flag}\
             --allowedTools 'Bash(*)' 'Write' 'Edit' \
             {append_sys_flag}\
             --max-turns 1000 \
             --output-format stream-json --verbose \
             < /dev/null 2>&1 | tee \"$LOG\"",
            invocation.timeout_secs
        ),
        Some(invocation.phase.append_system_prompt()),
    )?;

    Ok(status)
}

fn invoke_opencode(
    invocation: &AgentInvocation<'_>,
    log_path: &Path,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    info!(
        "Invoking OpenCode {} agent (model={}, timeout={}s, extra_env={} vars)",
        invocation.phase.label(),
        invocation.model.unwrap_or("(cli default)"),
        invocation.timeout_secs,
        invocation.extra_env.len()
    );

    let model_flag = invocation
        .model
        .map(|_| "--model \"$MODEL\" ")
        .unwrap_or_default();
    run_bash_agent(
        invocation,
        log_path,
        format!(
            "set -o pipefail; timeout {} opencode run \
             --format json \
             --thinking \
             --dangerously-skip-permissions \
             --pure \
             --agent {} \
             {model_flag}\
             \"$PROMPT\" \
             < /dev/null 2>&1 | tee \"$LOG\"",
            invocation.timeout_secs,
            invocation.phase.opencode_agent_name()
        ),
        None,
    )
}

fn run_bash_agent(
    invocation: &AgentInvocation<'_>,
    log_path: &Path,
    script: String,
    append_system_prompt: Option<&str>,
) -> Result<ExitStatus, Box<dyn std::error::Error>> {
    let openssl_dir = std::env::var("OPENSSL_DIR").unwrap_or_else(|_| "/usr".into());
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .env("PROMPT", invocation.prompt)
        .env("LOG", log_path)
        .env("OPENSSL_DIR", openssl_dir)
        .current_dir(invocation.work_dir);

    if let Some(system_prompt) = append_system_prompt {
        if !invocation.no_plan {
            cmd.env("APPEND_SYS", system_prompt);
        }
    }

    if let Some(model) = invocation.model {
        cmd.env("MODEL", model);
    }

    for (key, value) in invocation.extra_env {
        info!("Injecting env var: {key}");
        cmd.env(key, value);
    }

    if invocation.agent == AgentKind::Claude && claude_uses_ccr(invocation.model) {
        cmd.env("ANTHROPIC_BASE_URL", "http://127.0.0.1:3456");
    }

    Ok(cmd.status()?)
}

fn claude_uses_ccr(model: Option<&str>) -> bool {
    model.is_some_and(|m| m.contains(','))
}

/// Extract all unique session IDs from an OpenCode JSONL log file.
fn extract_session_ids_from_log(log_path: &Path) -> Vec<String> {
    let Ok(file) = fs::File::open(log_path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(sid) = val.get("sessionID").and_then(|v| v.as_str()) {
                if seen.insert(sid.to_string()) {
                    ids.push(sid.to_string());
                }
            }
        }
    }
    ids
}

/// Recursively extract sub-agent session IDs from an OpenCode export JSON.
/// Looks for `task` tool_use entries whose `metadata.sessionID` points to a child session.
fn extract_sub_session_ids_from_export(export_json: &serde_json::Value) -> Vec<String> {
    let mut ids = Vec::new();
    let Some(messages) = export_json.get("messages").and_then(|v| v.as_array()) else {
        return ids;
    };
    for msg in messages {
        let Some(parts) = msg.get("parts").and_then(|v| v.as_array()) else {
            continue;
        };
        for part in parts {
            let Some(tool_name) = part.get("tool").and_then(|v| v.as_str()) else {
                continue;
            };
            if tool_name != "task" {
                continue;
            }
            if let Some(sid) = part
                .pointer("/state/metadata/sessionID")
                .or_else(|| part.pointer("/state/metadata/sessionId"))
                .or_else(|| part.pointer("/state/metadata/session_id"))
                .and_then(|v| v.as_str())
            {
                if !sid.is_empty() {
                    ids.push(sid.to_string());
                }
            }
        }
    }
    ids
}

/// Export an OpenCode session by ID, returning the raw JSON string.
///
/// Known bug (opencode#14948): `opencode export` truncates JSON when stdout is
/// piped, but works correctly when redirected to a file.  We work around this
/// by redirecting stdout to a temp file instead of capturing it via pipe.
fn export_opencode_session(session_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let tmp = tempfile::NamedTempFile::new()?;
    let status = Command::new("opencode")
        .args(["export", session_id])
        .stdout(std::fs::File::create(tmp.path())?)
        .status()?;
    if !status.success() {
        return Err(format!("opencode export {session_id} failed (exit {status})").into());
    }
    let stdout = std::fs::read_to_string(tmp.path())?;
    let json_start = stdout.find('{').ok_or("opencode export produced no JSON")?;
    let raw = &stdout[json_start..];

    // Validate the JSON. If it is still broken (e.g. literal control chars
    // inside strings), return an error so the caller can fall back to JSONL.
    serde_json::from_str::<serde_json::Value>(raw)
        .map_err(|e| format!("opencode export {session_id} produced invalid JSON: {e}"))?;

    Ok(raw.to_string())
}
/// sub-agent sessions, appending each export block to `log_path` and
/// `output_log_path` (if set).
fn export_opencode_sessions(
    log_path: &Path,
    output_log_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_ids = extract_session_ids_from_log(log_path);
    if session_ids.is_empty() {
        info!("No session IDs found in OpenCode log; skipping export");
        return Ok(());
    }

    let mut exported: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = session_ids;

    while let Some(sid) = queue.pop() {
        if !exported.insert(sid.clone()) {
            continue;
        }
        info!("Exporting OpenCode session {sid}");
        match export_opencode_session(&sid) {
            Ok(json) => {
                let marker = format!("## opencode-export: {sid}\n");
                let block = format!("{marker}{json}\n");

                // Append to the per-agent log file.
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path)
                    .and_then(|mut f| {
                        use std::io::Write;
                        f.write_all(block.as_bytes())
                    })?;

                // Also append to the shared output log if configured.
                if let Some(out_path) = output_log_path {
                    fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(out_path)
                        .and_then(|mut f| {
                            use std::io::Write;
                            f.write_all(block.as_bytes())
                        })?;
                }

                // Write the export block to stderr (same stream as tracing log
                // messages) under a lock so the entire multi-line JSON object is
                // emitted atomically — no log lines can interleave mid-block.
                {
                    use std::io::Write;
                    let stderr = std::io::stderr();
                    let mut handle = stderr.lock();
                    let _ = handle.write_all(block.as_bytes());
                }

                // Discover sub-agent sessions from this export and enqueue them.
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) {
                    for child_sid in extract_sub_session_ids_from_export(&parsed) {
                        if !exported.contains(&child_sid) {
                            queue.push(child_sid);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to export OpenCode session {sid}: {e}");
            }
        }
    }
    Ok(())
}

fn append_trace_if_requested(
    log_path: &Path,
    output_log_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(out_path) = output_log_path else {
        return Ok(());
    };
    if !log_path.exists() {
        return Ok(());
    }

    match fs::read_to_string(log_path) {
        Ok(trace) => {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(out_path)
            {
                let _ = writeln!(file, "\n{}", trace);
                info!("Appended agent trace to {}", out_path.display());
            }
        }
        Err(e) => warn!(
            "Failed to read agent trace from {}: {}",
            log_path.display(),
            e
        ),
    }

    Ok(())
}

fn write_claude_sandbox(case_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let claude_dir = case_dir.join(".claude");
    fs::create_dir_all(&claude_dir)?;
    fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({
            "sandbox": {
                "enabled": true,
                "allowUnsandboxedCommands": false,
                "filesystem": {
                    "allowRead": [case_dir.to_string_lossy()],
                    "allowWrite": [case_dir.to_string_lossy()]
                }
            }
        })
        .to_string(),
    )?;
    Ok(())
}

struct OpenCodeAgentConfig<'a> {
    name: &'a str,
    description: &'a str,
    system_prompt: &'a str,
}

const OPENCODE_LOCAL_PERMISSIONS: &[(&str, &str)] = &[
    ("bash", "allow"),
    ("read", "allow"),
    ("edit", "allow"),
    ("write", "allow"),
    ("glob", "allow"),
    ("grep", "allow"),
    ("task", "allow"),
    ("todowrite", "allow"),
    ("lsp", "allow"),
    ("webfetch", "deny"),
    ("websearch", "deny"),
    ("skill", "deny"),
];

fn write_opencode_agent(
    work_dir: &Path,
    config: OpenCodeAgentConfig<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let agents_dir = work_dir.join(".opencode/agents");
    fs::create_dir_all(&agents_dir)?;

    let mut permissions = String::new();
    for (tool, policy) in OPENCODE_LOCAL_PERMISSIONS {
        permissions.push_str(&format!("  {tool}: {policy}\n"));
    }

    fs::write(
        agents_dir.join(format!("{}.md", config.name)),
        format!(
            "---\ndescription: {}\nmode: primary\npermission:\n{}---\n{}\n",
            config.description, permissions, config.system_prompt
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_ccr_detection_requires_comma_model() {
        assert!(claude_uses_ccr(Some("openrouter,deepseek/deepseek-v4-pro")));
        assert!(!claude_uses_ccr(Some("sonnet")));
        assert!(!claude_uses_ccr(None));
    }

    #[test]
    fn opencode_permissions_deny_web_and_skill() {
        assert!(OPENCODE_LOCAL_PERMISSIONS.contains(&("webfetch", "deny")));
        assert!(OPENCODE_LOCAL_PERMISSIONS.contains(&("websearch", "deny")));
        assert!(OPENCODE_LOCAL_PERMISSIONS.contains(&("skill", "deny")));
        assert!(OPENCODE_LOCAL_PERMISSIONS.contains(&("bash", "allow")));
    }

    #[test]
    fn extract_model_limits_matches_provider_and_id() {
        let sample = concat!(
            "opencode-go/mimo-v2.5-pro\n",
            "{\n",
            "  \"id\": \"mimo-v2.5-pro\",\n",
            "  \"providerID\": \"opencode-go\",\n",
            "  \"name\": \"MiMo V2.5 Pro\",\n",
            "  \"limit\": {\n",
            "    \"context\": 1048576,\n",
            "    \"output\": 128000\n",
            "  }\n",
            "}\n",
        );
        let limits = extract_model_limits_from_output(sample, "opencode-go", "mimo-v2.5-pro")
            .expect("limits must be found");
        assert_eq!(limits.context, 1_048_576);
        assert_eq!(limits.output, Some(128_000));
    }

    #[test]
    fn extract_model_limits_requires_exact_match() {
        let sample = concat!(
            "opencode-go/mimo-v2.5-pro\n",
            "{\n",
            "  \"id\": \"mimo-v2.5-pro\",\n",
            "  \"providerID\": \"opencode-go\",\n",
            "  \"limit\": {\n",
            "    \"context\": 1048576,\n",
            "    \"output\": 128000\n",
            "  }\n",
            "}\n",
        );
        assert!(extract_model_limits_from_output(sample, "opencode-go", "mimo-v2.5").is_none());
        assert!(
            extract_model_limits_from_output(sample, "other-provider", "mimo-v2.5-pro").is_none()
        );
    }
}
