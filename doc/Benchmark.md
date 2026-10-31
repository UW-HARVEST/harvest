# Benchmark
The `benchmark` tool is our custom integration-testing and benchmarking utility.
It takes as input a directory of benchmark programs (using the format described below) and produces an output directory containing translated Rust code, debugging information, and summary statistics.  

### Command-line Interface
The command-line interface of `benchmark` is:
```
Usage: benchmark [OPTIONS] <INPUT_DIR> <OUTPUT_DIR>

Arguments:
  <INPUT_DIR>   Path to the directory containing example subdirectories (each with test_case/ and test_vectors/)
  <OUTPUT_DIR>  Path to the output directory for all translated Rust projects

Options:
  -c, --config <CONFIG>              Set a configuration value; format $NAME=$VALUE
      --timeout <TIMEOUT>            Timeout in seconds for running test cases [default: 10]
      --filter <FILTER>              Filter benchmarks by regex pattern on directory names (keeps matching directories)
      --exclude <EXCLUDE>            Exclude benchmarks by regex pattern on directory names (removes matching directories)
      --feature-combos <MODE>        Feature combination mode [default: default]
  -h, --help                         Print help
```

This interface should be fairly intuitive.
The most important detail is that `benchmark` inherits all translation settings (e.g., LLM model choice) from your existing `translate` configuration file.
Only the input and output directories are determined by the command-line arguments.
If you need to override a configuration value, you can use the `--config` flag, which behaves exactly the same way as in translate.

#### Using the `--filter` and `--exclude` options
The `--filter` option takes a regular expression pattern and only runs benchmarks whose directory names match the pattern.
The `--exclude` option takes a regular expression pattern and excludes benchmarks whose directory names match the pattern.

**Note:** The `--filter` and `--exclude` options are mutually exclusive and cannot be used together.

#### Using the `--feature-combos` option

The `--feature-combos` option controls how many feature combinations are exercised when testing translated crates that have a `[features]` block in their `Cargo.toml`.

- `default` (default): Only the C build's default feature selection is tested. Behavior is identical to previous `benchmark` runs; the existing TRACTOR corpus is byte-for-byte unchanged.
- `all`: The full Cartesian product of features is tested. The product is capped at 1024; use `--feature-combos N` if you need a larger crate.
- `N` (positive integer): Up to N combinations are sampled evenly from the full Cartesian product (deterministic, no randomness). If the product has fewer than N entries, all are tested.

For crates without a `[features]` block, all modes behave identically to `default`.

**Filter examples:**

```bash
# Run only library benchmarks (directories ending with _lib)
benchmark Test-Corpus/Public-Tests/B01_synthetic output/ --filter=".*_lib$"

# Run only benchmarks starting with B01
benchmark Test-Corpus/Public-Tests output/ --filter="^B01"

# Run only benchmarks containing "example" in the name
benchmark Test-Corpus/Public-Tests output/ --filter=".*example.*"
```

**Exclude examples:**

```bash
# Exclude library benchmarks (directories ending with _lib)
benchmark Test-Corpus/Public-Tests/B01_synthetic output/ --exclude=".*_lib$"

# Exclude benchmarks starting with test_
benchmark Test-Corpus/Public-Tests output/ --exclude="^test_"

# Exclude benchmarks containing "skip" in the name
benchmark Test-Corpus/Public-Tests output/ --exclude=".*skip.*"
```


### Input Format
The expected directory structure of the `INPUT_DIR` of `benchmark` is as follows:
```
.
|-- 001_helloworld
|   |-- test_case
|   |   |-- <build files>
|   |   `-- src
|   |       `-- main.c
|   `-- test_vectors
|       |-- test1.json
|       |-- ...
|       `-- test3.json
|-- 002_stdin_echo
`-- ...
```
This mirrors the format used in the TRACTOR benchmark repository (e.g., `Test-Corpus/Public-Tests/B01_synthetic`). The layout is mostly self-explanatory, but additional documentation can be found in the TRACTOR repository's README.


### Output Format
The output directory structure of the `OUTPUT_DIR` of `benchmark` is as follows:
```
.
|-- output.log
|-- results.csv
|-- 001_helloworld/
|   |-- Cargo.toml
|   |-- src
|   |   `-- main.rs
|   |-- c_src
|   |   `-- main.c
|   |-- failed_tests
|   |   `-- test01.json
|   `-- results.err
|-- 002_stdin_echo/
`-- ...
```

- `output.log`: The raw output log, which includes the model used, the token budget, and fine-grained results (every test's result, including expected vs actual output).  
- `results.csv`: Summary statistics for the run, like build success rate and test success rate. See schema below.
- `Cargo.toml` and `src`: The translated rust code.   
- `c_src`: The original C source code, exactly copied from the input.  
- `failed_tests`: Any test cases that failed (copied from the input)  
- `results.err`: Error messages for any failing test cases.  

#### `results.csv` schema

Column positions are frozen -- downstream tooling depends on column order. Do not reorder without bumping a schema version.

| # | Column | Description |
|---|--------|-------------|
| 0 | `program_name` | Directory name of the benchmark case |
| 1 | `translation_success` | Whether translation produced a Cargo package |
| 2 | `rust_build_success` | Whether `cargo build` succeeded |
| 3 | `total_tests` | Number of test vectors |
| 4 | `passed_tests` | Number of test vectors that passed (default combo) |
| 5 | `skipped_tests` | Number of test vectors skipped |
| 6 | `success_rate` | Pass rate for the default combo (%) |
| 7 | `error_message` | First build/translation error, if any |
| 8 | `feature_combo` | Comma-separated enabled features, or `"default"` |
| 9 | `combo_passed` | Whether all test vectors passed for this combo |

With `--feature-combos default` there is exactly one row per program (`feature_combo="default"`). With `all` or `N` there is one row per exercised combination. Downstream tooling should group by `program_name` and aggregate `combo_passed` for a strict all-combos pass rate.
