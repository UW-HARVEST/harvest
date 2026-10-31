<!-- markdownlint-disable MD041 -->
Translate the C code in c_src/ to Rust that produces **byte-identical output** for the same inputs.
A project scaffold is **already provided** in the current directory (the Rust project root, NOT inside
`c_src/`): a `Cargo.toml` (crate name, `[lib]`/`[[bin]]` target, any `[features]` block), a `build.rs`
(if configurable), and a `rust-toolchain.toml`. Do **NOT** modify, recreate, or delete these files.
Write only the Rust source files under `src/`. Read the provided `Cargo.toml` first for the exact crate
name and target; reference items via `crate::`/`mod` and never invent a crate name.

**CRITICAL CONSTRAINT: Pure Rust Translation Only**

You MUST faithfully translate ALL C source files to **pure Rust**. Do NOT use
the `cc` crate (or any equivalent) in `build.rs` to compile or link the original
C source code. The C source files in `c_src/` will NOT exist in the final test
environment — the only code available at test time is the Rust you write. Any
`extern "C"` FFI declarations must resolve to Rust implementations you provide,
not to compiled C object files.

Do NOT import or depend on any existing Rust crate that implements, wraps,
re-exports, or compiles the same C library you are translating. Every line
of Rust code must be written by you. If a function needs to call out to
system libraries (e.g. POSIX APIs), use `libc` or equivalent thin FFI crates,
not crates that compile the library you are meant to translate.

A `build.rs` is allowed for legitimate build-time needs (code generation,
feature detection, etc.), but it must NOT reference, compile, or link any
file under `c_src/`.

If the codebase is large, you must still translate all of it. No stubs, no
placeholders, no shortcuts. Never refuse or abort a translation because the codebase seems too big.

## Step 1: Analyze BEFORE writing any code

Before writing a single line of Rust, you MUST:
1. Read `c_src/CMakeLists.txt` to understand the build system, source file selection,
   and any build-time configurability (cache variables, options, conditional compilation)
2. Read all header files to understand the public API, preprocessor macros, and
   namespace/symbol renaming patterns
3. The project type is fixed by the provided `Cargo.toml`: read it to see whether the target is a
   `[[bin]]` (`name = "driver"`, write `src/main.rs`) or a `[lib]` (write `src/lib.rs`). Do not change it.
4. Identify ALL backends/variants if the project has build-time configurability

## Step 2: Plan the translation

If the project has build-time configurability (CMake cache variables selecting different
source files or parameters):
- You MUST preserve this using **Cargo features**. The provided `Cargo.toml` already contains the
  `[features]` block and a `build.rs` — do NOT modify them. For an enum variable `VAR` with values
  `a`, `b` the features are named `VAR_a`, `VAR_b`; gate code with the **bare cfg** `#[cfg(VAR_a)]`
  (NOT `#[cfg(feature = "VAR_a")]`). For a boolean variable `VAR`, gate with `#[cfg(feature = "VAR")]`.
- ALL feature combinations must compile.
- Do NOT hardcode a single configuration — every variant must be implemented, and do NOT read a
  variable's value via `env!(...)`.

For large projects, break the work into phases: shared/core code first, then each
backend or variant, then wire up feature gates.

## Step 3: Translate

- All public C functions must use `#[unsafe(no_mangle)]` and `extern "C"` with exact
  C signatures (use `*const c_char`, `c_int`, etc. from `std::ffi`)
- Pay attention to C preprocessor macros that RENAME functions (e.g.,
  `#define foo NAMESPACE(foo)` makes the linker symbol `PREFIX_foo`, not `foo`).
  The Rust `#[no_mangle]` name must match the FINAL linker symbol, not the
  source-level name.
- Do NOT fix bugs in the original C code — reproduce behavior exactly
- Preserve the exact order of error checks and validation
- Match C's stdin reading behavior exactly (scanf reads across newlines, fgets does not)
- Match C's exact printf format output including spacing and newlines
- Do NOT use the `openssl` crate or any OpenSSL bindings — use pure-Rust crates instead
- Use safe Rust internally where possible

## Step 4: Verify

Run `cargo build --release` and fix any errors until it compiles.
If the project has Cargo features, verify ALL feature combinations compile:
run `cargo build --release --features <feature>` for each one. The exact feature names are those in
the provided `Cargo.toml`.

Do NOT modify anything in c_src/.
