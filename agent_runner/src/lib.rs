use harvest_core::config::AgentKind;
use std::collections::HashMap;
use std::fs;
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
}
