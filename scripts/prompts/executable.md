<!-- markdownlint-disable MD041 -->
Translate the C code in c_src/ to an idiomatic Rust Cargo project.
Write Cargo.toml and src/ files in the current directory (NOT in c_src/).
This is an EXECUTABLE. Requirements:
- The binary name in Cargo.toml must be "driver" (use [[bin]] name = "driver")
- Preserve the exact I/O behavior (same stdout output for same inputs)
- Use safe Rust internally where possible
Run 'cargo build --release' and fix any errors until it compiles.
Do NOT modify anything in c_src/.
