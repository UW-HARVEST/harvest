# How to add a new tool
Harvest works by running a sequence of tools. These tools can really do anything (tools are used to implement translation passes, parse the C ast, build the project, and eventually we intend to have tools to test the project as well). Adding a new tool to run in Harvest's pipeline requires 4 steps: ...

### Step 1: Add the tool to Cargo.toml

Select a name for your new tool. This document's examples will use "my new
tool". Add your tool as a new sub-create in the `tools/` directory with its own manifest at `tools/my_new_tool/Cargo.toml`: 

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
and include it in the project `Cargo.toml`:
```toml
[workspace]
// (1) Add tool as member of Harvest workspace
members = ["core", "tools/load_raw_source", "tools/my_new_tool"] 

[workspace.dependencies]
harvest_core = { path = "core" }
load_raw_source = { path = "tools/load_raw_source" }
// (2) Add new crate for the tool
my_new_tool = { path = "tools/my_new_tool" }
```

Next, implement `Representation` and `Tool`

### Step 2: Declare the type of output the tool produces --- its `Representation`
The output of Tools in Harvest are representations. These representations could be anything from a C ast to a single bool that tells the user that building the translated project suceeded/failed. To implement a representation, we declare the data layout of the representation, give it a name, and give it a method to be written to disk.
An example is described below:

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

### Step 3: Implement the `Tool` trait to create your `Representation` 
Next, we write the actual tool logic by Implementing the `Tool` trait. 
This mainly entails implementing the `run` method, which takes as input a vector of input identifiers, which allow us to retrieve data from other representations (see Step 4 and existing tool implementations for examples). The output of `Run` is the Representation, and Harvest's schedular will automatically persistently store the result of `run` and make it available to other Tools.

```Rust
pub struct MyNewTool {
}

impl Tool for LoadRawSource {
    fn name(&self) -> &'static str {
        "my_new_tool"
    }

    fn run(
        self: Box<Self>,
        _context: RunContext,
        _inputs: Vec<Id>,
    ) -> Result<Box<dyn Representation>, Box<dyn std::error::Error>> {
        // Actual Tool logic goes here..
        Ok(Box::new(RawSource { result: "Hello World!".to_string() }))
    }
}
```


### Step 4: Schedule your tool to run
Finally, its time to actually run our tool.
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

And then in the implementation of `run`: 

```Rust
pub struct MyNewTool {
}

impl Tool for LoadRawSource {
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

        Ok(Box::new(RawSource { result: "Hello World!".to_string() }))
    }
}
```

