## Step 2: Verification workflow (libloading)

You compare C against Rust from **inside a Rust integration test**: build the C
code as a shared library, `dlopen` it with the `libloading` crate, and call the
C and Rust implementations side by side on the same inputs.

Build the C code as a shared library first. Look at `c_src/CMakeLists.txt` to
understand the build system, then:

```
cd c_src && mkdir -p build && cd build && \
cmake .. -DCMAKE_POSITION_INDEPENDENT_CODE=ON {CMAKE_BUILD_FLAGS} && \
cmake --build .
```

Find the resulting `.so` files in the build output.

Then do the actual verification:

1. Add `libloading = "0.8"` to `[dev-dependencies]` in `Cargo.toml` (so your
   integration tests can dlopen the C shared library).
2. Write Rust integration tests (in `tests/`) that use `libloading` to load the
   C `.so` and compare C vs Rust function outputs.
3. Start with the lowest-level functions and work upward to higher-level ones.
   Look at the C headers to identify the public API and function call hierarchy.
4. For each function: create fixed test inputs, call both C and Rust versions,
   assert outputs match byte-for-byte.
5. Run `cargo test` and investigate any mismatches. Every time a test exposes a
   divergence, append a hypothesis to `HYPOTHESES.md`.
6. When you find a Rust function that produces different output than C, fix the
   Rust code in `src/` and re-run until the test passes. Update the matching
   hypothesis to `fixed` after the Edit.
7. Keep going until all public functions match.
8. If the project has a main binary, run both the C binary and the Rust binary
   with the same inputs and compare their stdout byte-for-byte. Fix any
   differences.
9. Compare `nm -D` on the C `.so` and the Rust `.so`. Every symbol the C `.so`
   exports, the Rust `.so` must also export with the exact same name. This
   includes symbols created by preprocessor macros. If the C `.so` exports it,
   the Rust `.so` must export it — no exceptions. Add missing exports.
