# Adding a New Tool

Harvest executes a pipeline of tools.
Each tool performs a single transformation or analysis step and produces an output called a **representation**. Examples include:

* loading raw C source files
* parsing a C AST
* translation passes
* running `cargo build` the generated project
* (eventually) testing the translated project

A tool can do anything, as long as it consumes existing representations and produces a new one.

Adding a new tool to the Harvest pipeline consists of four steps:
1. Add a crate for the tool
2. Define the output `Representation`
3. Implement the `Tool` logic
4. Schedule the tool in the pipeline

---

### Step 1: Add the tool to Cargo.toml

Select a name for your new tool. 
This document's examples will use `my_new_tool`. 
Add your tool as a new sub-crate in the `tools/` directory with its own manifest at `tools/my_new_tool/Cargo.toml`: 

```toml
[package]
name = "my_new_tool"
version = "0.1.0"
edition = "2024"

[dependencies]
harvest_core.workspace = true

[lints]
workspace = true
```

and include it in the base Harvest `Cargo.toml`:
```toml
[workspace]
# (1) Add tool as member of Harvest workspace
members = ["core", "tools/load_raw_source", "tools/my_new_tool"] 

[workspace.dependencies]
harvest_core = { path = "core" }
load_raw_source = { path = "tools/load_raw_source" }
# (2) Add new crate for the tool
my_new_tool = { path = "tools/my_new_tool" }
```

At this point the tool should build, but do nothing.

---

### Step 2: Define the Tool’s Output (Representation)

Every tool produces exactly one **representation**.
A representation is simply structured data stored in Harvest’s IR.

Representations may be large (e.g., a parsed AST) or tiny (e.g., a `bool` indicating build success).

To define a representation:

1. Define the data structure
2. Implement the `Representation` trait which requires defining the `name` method, which is self-explanatory and the  `materialize` method, which stores this representation to disk.


Here's how we implement `MyRepresentation`, the `Representation` for `my_new_tool`:

```rust
pub struct MyRepresentation {
    pub result: String,
}


impl Representation for MyRepresentation {
    fn name(&self) -> &'static str {
        "cargo_build_result"
    }

    fn materialize(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}
```
---
### Step 3: Implement the `Tool` 
Next, we write the actual tool logic by Implementing the `Tool` trait. 
This mainly entails implementing the `run` method, which takes as input a vector of input identifiers, which are handles to other previously computed representations (see Step 4 for how to retrieve representations). 
The output of `run` is the Representation, and Harvest's scheduler will store the result of `run` and make it available to other Tools.

Here is how we implement the `Tool` trait for `my_new_tool`:

```Rust
pub struct MyNewTool {
}

impl Tool for MyNewTool {
    fn name(&self) -> &'static str {
        "my_new_tool"
    }

    fn run(
        self: Box<Self>,
        _context: RunContext,
        _inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Actual Tool logic goes here..
        Ok(Box::new(MyNewTool { result: "Hello World!".to_string() }))
    }
}
```
At this point, we have a working tool, but we need to tell Harvest to schedule the tool to run during transpilation.

---

### Step 4: Schedule your tool to run
Finally, it's time to actually run our tool.
To do this, we add a new `queue` call to the main `transpile` function in `translate/src/lib.rs`

```rust
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();

    // Setup a schedule for the transpilation.
    let _ = scheduler.queue(MyNewTool);
 
    // Run all scheduled tools
    let result = scheduler.run_all(&mut runner, &mut ir, config);
    // ...
}
```

This is a very simple example, since `MyNewTool` takes no inputs and its outputs are not used.
If `MyNewTool` for example needed access to the Raw C source code, we could schedule `MyNewTool` to run after the already-existing `LoadRawSource` tool as such:

```rust
pub fn transpile(config: Arc<Config>) -> Result<HarvestIR, Box<dyn std::error::Error>> {
    // Basic tool setup
    let collector = diagnostics::Collector::initialize(&config)?;
    let mut ir = HarvestIR::default();
    let mut runner = ToolRunner::new(collector.reporter());
    let mut scheduler = Scheduler::default();


    // Setup a schedule for the transpilation.
    let load_src = scheduler.queue(LoadRawSource::new(&config.input));
    let _ = scheduler.queue_after(MyNewTool, &[load_src]);
 
    // Run until all tasks are complete, respecting the dependencies declared in `queue_after`
    let result = scheduler.run_all(&mut runner, &mut ir, config);
    // ...
}
```
Here, `scheduler.run_all` will track the dependencies declared in `queue_after` and ensure that `MyNewTool` runs after `LoadRawSource`. The scheduler is smart enough to run tools that do not depend on one another (directly or transitively) in parallel, so there is no need to explicitly parallelize tools.

To use the `RawSource` `Representation` inside the `run` method you can call `context.ir_snapshot.get`: 

```Rust
pub struct MyNewTool {
}

impl Tool for MyNewTool {
    fn name(&self) -> &'static str {
        "my_new_tool"
    }

    fn run(
        self: Box<Self>,
        context: RunContext,
        inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Fetch the input representation
        let _source = context
            .ir_snapshot
            .get::<RawSource>(inputs[0])
            .ok_or("No RawSource representation found in IR")?;

        Ok(Box::new(MyNewTool { result: "Hello World!".to_string() }))
    }
}
```
---

### Summary

To add a tool:

1. Create a crate in `tools/`
2. Define a `Representation`
3. Implement `Tool::run`
4. Schedule it with `queue` or `queue_after`

After that, Harvest automatically handles IR storage, dependency tracking, and parallel execution.

---
