//! Individual tools (and their interfaces) used by HARVEST to translate C to Rust.

use crate::config::Config;
use crate::diagnostics::ToolReporter;
use crate::{HarvestIR, Id, Representation};
use std::sync::Arc;

/// Trait implemented by each tool. Used by the scheduler to decide what tools
/// to run and to manage those tools.
///
/// An instance of Tool represents a particular invocation of that tool (i.e.
/// certain arguments and a certain initial IR state). The scheduler -- or other code -- constructs
/// a Tool when it is considering running that tool. The scheduler then decides whether to invoke
/// the tool based on which parts of the IR it writes.
///
/// The tool's constructor does not appear in the Tool trait, because at the
/// time the scheduler constructs the tool it is aware of the tool's concrete
/// type.
pub trait Tool: Send + 'static {
    /// This tool's name. Should be snake case, as this will be used to create directory and/or
    /// file names.
    fn name(&self) -> &'static str;

    /// Runs the tool logic. IR access and edits are made using `context`.
    ///
    /// If `Ok` is returned the changes will be applied to the IR, and if `Err`
    /// is returned the changes will not be applied.
    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>>;
}

/// Context a tool is provided when it is running. The tool uses this context to
/// access the IR, make IR changes, launch external processes (with
/// diagnostics), and anything else that requires hooking into the rest of
/// harvest_translate.
#[non_exhaustive]
pub struct RunContext {
    /// Read access to the IR.
    pub ir_snapshot: Arc<HarvestIR>,

    /// Configuration for the current harvest_translate run.
    pub config: Arc<Config>,

    /// Handle through which to report diagnostics and create temporary directories (which live
    /// inside the diagnostics directory).
    pub reporter: ToolReporter,
}

impl RunContext {
    /// Creates a new RunContext.
    pub fn new(
        ir_snapshot: Arc<HarvestIR>,
        config: Arc<Config>,
        reporter: ToolReporter,
    ) -> RunContext {
        RunContext {
            ir_snapshot,
            config,
            reporter,
        }
    }
}
