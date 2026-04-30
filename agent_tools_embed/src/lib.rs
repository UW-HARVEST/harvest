//! Embeds agent-facing tool scripts and their usage docs into the binary.
//!
//! At runtime, call [`materialize_to`] to write scripts into the agent's working
//! directory, and [`collect_docs`] to get a concatenated markdown string suitable
//! for injection into an agent prompt via the `{AGENT_TOOLS_DOCS}` placeholder.
//!
//! Both the scripts and the docs may contain `{AGENT_TOOLS_DIR}` — replace it
//! with the absolute path of the materialized directory before passing to the agent.

use std::fs;
use std::path::Path;

pub struct AgentTool {
    /// Script filename to write into the agent's tool directory.
    /// `None` for system tools that are already on PATH (doc-only entries).
    pub filename: Option<&'static str>,
    pub script: Option<&'static str>,
    pub doc: &'static str,
}

pub const ALL_TOOLS: &[AgentTool] = &[
    AgentTool {
        filename: Some("c_sandbox.py"),
        script: Some(include_str!("../../agent_tools/c_sandbox.py")),
        doc: include_str!("../../agent_tools/c_sandbox.md"),
    },
    AgentTool {
        filename: Some("symscan.py"),
        script: Some(include_str!("../../agent_tools/symscan.py")),
        doc: include_str!("../../agent_tools/symscan.md"),
    },
    AgentTool {
        filename: Some("callgraph.py"),
        script: Some(include_str!("../../agent_tools/callgraph.py")),
        doc: include_str!("../../agent_tools/callgraph.md"),
    },
    AgentTool {
        filename: None,
        script: None,
        doc: include_str!("../../agent_tools/unifdef.md"),
    },
];

/// Write all tool scripts into `dir`. Creates `dir` if it does not exist.
/// Doc-only tools (no script) are skipped silently.
pub fn materialize_to(dir: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(dir)?;
    for tool in ALL_TOOLS {
        if let (Some(filename), Some(script)) = (tool.filename, tool.script) {
            fs::write(dir.join(filename), script)?;
        }
    }
    Ok(())
}

/// Return a markdown string containing the usage docs for all tools,
/// separated by horizontal rules. The string still contains the
/// `{AGENT_TOOLS_DIR}` placeholder — replace it before injecting into a prompt.
pub fn collect_docs() -> String {
    ALL_TOOLS
        .iter()
        .map(|t| t.doc)
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}
