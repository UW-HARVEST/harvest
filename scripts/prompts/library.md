Translate the C code in c_src/ to an idiomatic Rust Cargo project.
Write Cargo.toml and src/ files in the current directory (NOT in c_src/).
This is a LIBRARY. Requirements:
- Cargo.toml must have crate-type = ["cdylib"] under [lib]
- All public C functions must use #[unsafe(no_mangle)] and extern "C"
- Preserve the exact C function signatures (use *const c_char, c_int, etc. from std::ffi)
- Use safe Rust internally where possible
Run 'cargo build --release' and fix any errors until it compiles.
Do NOT modify anything in c_src/.
