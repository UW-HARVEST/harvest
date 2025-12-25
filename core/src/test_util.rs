//! Place to put utilities that are only used by tests.

use crate::tools::{MightWriteContext, MightWriteOutcome, RunContext, Tool};
use std::error::Error;

/// Returns a new temporary directory. Unlike the defaults in the `tempdir` and `tempfile` crates,
/// this directory is not world-accessible by default.
#[cfg(not(miri))]
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    use std::fs::Permissions;
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

/// A tool that can be programmed to have many different behaviors, for testing code that calls
/// `Tool`'s methods.
pub struct MockTool {
    name: &'static str,
    might_write: Box<dyn FnMut(MightWriteContext) -> MightWriteOutcome + Send>,
    #[allow(clippy::type_complexity)]
    run: Box<dyn FnOnce(RunContext) -> Result<(), Box<dyn Error>> + Send>,
}

impl Default for MockTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder-style API for configuring how this MockTool behaves.
///
/// # Example
/// ```
/// use harvest_core::test_util::MockTool;
/// use harvest_core::tools::MightWriteOutcome;
/// let tool = MockTool::new()
///     .might_write(|_| MightWriteOutcome::Runnable([].into()))
///     .run(|_| Ok(()));
/// ```
#[cfg_attr(miri, allow(unused))]
impl MockTool {
    /// Creates a new MockTool.
    pub fn new() -> MockTool {
        MockTool {
            name: "mock_tool",
            might_write: Box::new(|_| MightWriteOutcome::Runnable([].into())),
            run: Box::new(|_| Ok(())),
        }
    }

    /// Returns this MockTool in a box. For use when a `Box<dyn Tool>` is needed.
    pub fn boxed(self) -> Box<MockTool> {
        self.into()
    }

    /// Sets a closure to be run when `Tool::might_write` is called.
    pub fn might_write<F: FnMut(MightWriteContext) -> MightWriteOutcome + Send + 'static>(
        mut self,
        f: F,
    ) -> MockTool {
        self.might_write = Box::new(f);
        self
    }

    /// Sets the return value of `Tool::name`.
    pub fn name(mut self, name: &'static str) -> MockTool {
        self.name = name;
        self
    }

    /// Sets a closure to be run when `Tool::run` is called.
    pub fn run<F: FnOnce(RunContext) -> Result<(), Box<dyn Error>> + Send + 'static>(
        mut self,
        f: F,
    ) -> MockTool {
        self.run = Box::new(f);
        self
    }
}

impl Tool for MockTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn might_write(&mut self, context: MightWriteContext) -> MightWriteOutcome {
        (self.might_write)(context)
    }

    fn run(self: Box<Self>, context: RunContext) -> Result<(), Box<dyn Error>> {
        (self.run)(context)
    }
}
