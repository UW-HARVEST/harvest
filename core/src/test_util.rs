//! Place to put utilities that are only used by tests.

use crate::tools::{RunContext, Tool};
use crate::{Id, Representation};
use std::error::Error;
use std::path::Path;

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
    #[allow(clippy::type_complexity)]
    run: Box<
        dyn FnOnce(RunContext, Vec<Id>) -> Result<Box<dyn Representation>, Box<dyn Error>> + Send,
    >,
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
/// use harvest_core::test_util::{MockTool, MockRepresentation};
/// let tool = MockTool::new()
///     .run(|_,_| Ok(Box::new(MockRepresentation)));
/// ```
#[cfg_attr(miri, allow(unused))]
impl MockTool {
    /// Creates a new MockTool.
    pub fn new() -> MockTool {
        MockTool {
            name: "mock_tool",
            run: Box::new(|_, _| Ok(Box::new(MockRepresentation))),
        }
    }

    /// Returns this MockTool in a box. For use when a `Box<dyn Tool>` is needed.
    pub fn boxed(self) -> Box<MockTool> {
        self.into()
    }

    /// Sets the return value of `Tool::name`.
    pub fn name(mut self, name: &'static str) -> MockTool {
        self.name = name;
        self
    }

    /// Sets a closure to be run when `Tool::run` is called.
    pub fn run<
        F: FnOnce(RunContext, Vec<Id>) -> Result<Box<dyn Representation>, Box<dyn Error>>
            + Send
            + 'static,
    >(
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

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn Error>> {
        (self.run)(context, inputs)
    }
}

pub struct MockRepresentation;

impl Representation for MockRepresentation {
    fn name(&self) -> &'static str {
        "mock_representation"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Display for MockRepresentation {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "MockRepresentation")
    }
}
