# How to add a new tool

## New file and type

Select a name for your new tool. This document's examples will use "my new
tool". Add your tool into `translate/src/tools/mod.rs` as a new public module:

```rust
pub mod load_raw_source;
pub mod my_new_tool; // Newly-added line
pub mod raw_source_to_cargo_llm;
pub mod try_cargo_build;
```

Then create a source code file for your new module at
`translate/src/tools/<tool name>.rs`. In that source file, add a type for your
tool (this will probably be a struct), and implement the
`harvest_translate::tool::Tool` trait:

```rust
use crate::tools::{MightWriteContext, MightWriteOutcome, RunContext, Tool};
use tracing::info;

/// A new tool which represents any program as the message "Hello, World!".
pub struct MyNewTool {}

impl MyNewTool {
    pub fn new() -> MyNewTool {
        MyNewTool {}
    }
}

impl Tool for MyNewTool {
    fn name(&self) -> &'static str {
        "my_new_tool"
    }

    fn might_write(&mut self, _context: MightWriteContext) -> MightWriteOutcome {
        // Indicate this tool is always runnable and will not edit any existing representations.
        MightWriteOutcome::Runnable([].into())
    }

    fn run(self: Box<Self>, _context: RunContext) -> Result<(), Box<dyn std::error::Error>> {
        info!("MyNewTool running");
        // TODO: Add a new "Hello, World!" representation
        Ok(())
    }
}
```

We also need to tell the scheduler to run our new tool. To do this, add a new
`scheduler.queue_invocation` call to `transpile` in `translate/src/lib.rs`:

```rust
pub fn transpile(config: Arc<cli::Config>) -> Result<Arc<HarvestIR>, Box<dyn std::error::Error>> {
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir_organizer = edit::Organizer::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();
    scheduler.queue_invocation(LoadRawSource::new(&config.input));
    scheduler.queue_invocation(RawSourceToCargoLlm);
    scheduler.queue_invocation(TryCargoBuild);
    scheduler.queue_invocation(tools::my_new_tool::MyNewTool::new()); // Newly-added
    loop {
        let snapshot = ir_organizer.snapshot();
// ...
```

Now, if we run `translate`, our new tool is executed:

```
$ RUST_LOG="info" cargo run --bin translate --release
   Compiling harvest_translate v0.1.0 (/home/ryan/harvest/code/translate)
    Finished `release` profile [optimized] target(s) in 1.80s
     Running `target/release/translate`
[2025-12-02T20:33:38Z INFO  harvest_translate] Launched tool load_raw_source
[2025-12-02T20:33:38Z INFO  harvest_translate] Launched tool my_new_tool
[2025-12-02T20:33:38Z INFO  harvest_translate::tools::my_new_tool] MyNewTool running
```

## Adding a Representation

The tool doesn't modify the IR yet. Lets add a new type of representation. To do
so, add a new type, and implement the `harvest_ir::Representation` trait:

```rust
use harvest_ir::Representation;
use std::fmt::{self, Display, Formatter};

pub struct HelloMessage {
    recipient: String,
}

// Representation requires that Display be implemented.
impl Display for HelloMessage {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Hello, {}!", self.recipient)
    }
}

impl Representation for HelloMessage {
    fn name(&self) -> &'static str {
        "HelloMessage"
    }
}
```

We then modify `<MyNewTool as Tool>::run` to use the `harvest_ir::Edit`
available at `context.ir_edit` to add the representation:

```rust
    fn run(self: Box<Self>, context: RunContext) -> Result<(), Box<dyn std::error::Error>> {
        info!("MyNewTool running");
        context.ir_edit.add_representation(Box::new(HelloMessage { recipient: "World".to_owned() }));
        Ok(())
    }
```

If we run translate again with a diagnostics directory, we can see that the IR
now has a HelloMessage representation:

```
$ cargo run --bin translate --release -- --config diagnostics_dir=../diagnostics
$ ls ../diagnostics/ir/
001  002  003  004
$ cat ../diagnostics/ir/004/index  # The highest-numbered IR is the last
001: HelloMessage
002: RawSource
003: CargoPackage
004: CargoBuildResult
$ cat ../diagnostics/ir/004/001    # 001 is the ID with the HelloMessage repr
Hello, World!
```
