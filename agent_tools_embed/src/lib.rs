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
    pub filename: &'static str,
    pub script: &'static str,
    pub doc: &'static str,
}

pub const ALL_TOOLS: &[AgentTool] = &[
    AgentTool {
        filename: "c_sandbox.py",
        script: include_str!("../../agent_tools/c_sandbox.py"),
        doc: include_str!("../../agent_tools/c_sandbox.md"),
    },
    AgentTool {
        filename: "symbol_diff.py",
        script: include_str!("../../agent_tools/symbol_diff.py"),
        doc: include_str!("../../agent_tools/symbol_diff.md"),
    },
];

/// Write all tool scripts into `dir`. Creates `dir` if it does not exist.
pub fn materialize_to(dir: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(dir)?;
    for tool in ALL_TOOLS {
        fs::write(dir.join(tool.filename), tool.script)?;
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
