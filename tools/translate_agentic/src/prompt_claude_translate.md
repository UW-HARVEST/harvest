<!-- markdownlint-disable MD041 -->
Translate the C code in c_src/ to Rust that produces **byte-identical output** for the same inputs.

A project scaffold is **already provided** in the current directory: a `Cargo.toml`
(with the crate name, the `[lib]`/`[[bin]]` target, and any `[features]` block),
a `build.rs` (if the project is configurable), and a `rust-toolchain.toml`. Do
**NOT** modify, recreate, or delete these files. Write only the Rust source files
under `src/`. Read the provided `Cargo.toml` first to learn the exact crate name
and targets — reference items within the crate via `crate::`/`mod`, and if you
must name the crate (e.g. from a `[[bin]]` target) use the exact name from that
`Cargo.toml`. Do not invent a different crate name.

## Step 1: Analyze BEFORE writing any code

Before writing a single line of Rust, you MUST:
1. Read `c_src/CMakeLists.txt` to understand the build system, source file selection,
   and any build-time configurability (cache variables, options, conditional compilation)
2. Read all header files to understand the public API, preprocessor macros, and
   namespace/symbol renaming patterns
3. Read the provided `Cargo.toml` to learn the crate name and whether the target
   is a `[[bin]]` (`name = "driver"`), a `[lib]`, or both — this is already set
   for you; match your `src/` layout to it (`src/main.rs` for a bin, `src/lib.rs`
   for a lib).
4. Identify ALL backends/variants if the project has build-time configurability

## Step 2: Plan the translation

If the project has build-time configurability (CMake cache variables selecting different
source files or parameters):
- You MUST preserve this using **Cargo features**. The provided `Cargo.toml`
  already contains the `[features]` block and a `build.rs` — do NOT modify them.
  Use the feature names exactly as written there. For enum-kind variables
  (`VAR` with values `a`, `b`), gate code with bare cfg attributes such as
  `#[cfg(VAR_a)]` (NOT `feature = "VAR_a"`). For boolean variables `VAR`,
  gate with `#[cfg(feature = "VAR")]` using the variable's exact case.
- ALL combinations of features must compile.
- Plan which source files map to which features before writing code.
- Do NOT hardcode a single configuration -- every variant must be implemented.

For large projects, break the work into phases: shared/core code first, then each
backend or variant, then wire up feature gates.

## Step 3: Translate

- All public C functions must use `#[unsafe(no_mangle)]` and `extern "C"` with exact
  C signatures (use `*const c_char`, `c_int`, etc. from `std::ffi`)
- Pay attention to C preprocessor macros that RENAME functions (e.g.,
  `#define foo NAMESPACE(foo)` makes the linker symbol `PREFIX_foo`, not `foo`).
  The Rust `#[no_mangle]` name must match the FINAL linker symbol, not the
  source-level name.
- Do NOT fix bugs in the original C code -- reproduce behavior exactly
- Preserve the exact order of error checks and validation
- Match C's stdin reading behavior exactly (scanf reads across newlines, fgets does not)
- Match C's exact printf format output including spacing and newlines
- Do NOT use the `openssl` crate or any OpenSSL bindings -- use pure-Rust crates instead
- Use safe Rust internally where possible

## Step 4: Verify

Run `cargo build --release` and fix any errors until it compiles.
If the project has Cargo features, verify ALL feature combinations compile:
run `cargo build --release --features <feature>` for each one.

## Waiting on long-running commands

Builds and reference-output generation can be slow. When you need to wait for a
long command, run it with `run_in_background` and poll for completion, or wrap a
short sleep in a condition loop (e.g. `until [ -f build.done ]; do sleep 2; done`).
Do NOT block on a single long foreground `sleep` such as `sleep 30 && cat log` --
it will be rejected, and chaining `sleep` calls only wastes turns.

Do NOT modify anything in c_src/.
