# HARVEST code

A place to put HARVEST code that has not yet been migrated into its own
repository.

## Building the Rust code

If you have [rustup](https://rustup.rs) installed, you can build the code by
running:

```bash
cargo build --release
```

If you do not use rustup, you will need a sufficiently-new stable Rust compiler
(see rust-toolchain.toml for a toolchain version that is known to work).

## LLM server

You will also need an LLM server. This can be local, or remote. A couple options
are given below:

### Local Ollama instance

You can follow Ollama's [download instructions](https://ollama.com/download), or
download its [Docker image](https://hub.docker.com/r/ollama/ollama).

Once you have it installed, you need to download a model. By default,
harvest_translate uses `codellama:7b`:

```bash
ollama pull codellama:7b                       # If installed in your system
docker container run ollama pull codellama:7b  # If using Docker
```

You will need to have Ollama running to run harvest_translate.

### Remote OpenAI instance

First, you'll need to provision an [OpenAI API key](https://platform.openai.com/api-keys).

Then, you'll need to set up a custom Harvest config file:
```toml 
[tools.raw_source_to_cargo_llm]
backend = "openai"
model = "gpt-4o"
api_key = "your_key_here" # Will be read from environment if empty
address = ""  # Not needed for OpenAI
max_tokens = 16384
```
You should place this config at the OS-dependent harvest config location, which you can find by running:
```bash
cargo run -- --print-config-path
``` 


## Running

### Translate C code to Rust
```bash
cargo run --bin=translate --release -- /path/to/c/code -o /path/to/output
# Example:
# cargo run --bin=translate --release -- Test-Corpus/Public-Tests/B01_synthetic/001_helloworld/test_case/ -o example_output/
```

### Running a set of TRACTOR benchmarks
```bash
cargo run --bin=benchmark --release -- /path/to/input/dir /path/to/output/dir
# Example: run all benchmarks
# cargo run --bin=benchmark --release -- Test-Corpus/Public-Tests/B01_synthetic example_output/

# Optional: add --filter=<regex> to filter benchmarks by directory name (only matching directories will run)
# Example: run only library benchmarks (directories ending with _lib)
# cargo run --bin=benchmark --release -- Test-Corpus/Public-Tests/B01_synthetic example_output/ --filter=".*_lib$"

# Example: run only executable benchmarks (exclude directories ending with _lib)
# cargo run --bin=benchmark --release -- Test-Corpus/Public-Tests/B01_synthetic example_output/ --filter="^(?!.*_lib$)"

# Example: run only benchmarks starting with B01
# cargo run --bin=benchmark --release -- Test-Corpus/Public-Tests example_output/ --filter="^B01"
```

### Configuration
Print config file location:
```bash
cargo run --bin=translate -- --print-config-path
```

You can find more information on configuration in [docs/Configuration.md].
