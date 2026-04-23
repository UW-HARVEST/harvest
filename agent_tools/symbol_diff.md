### Tool: `symbol_diff` — Compare exported symbols between a C .so and a Rust .so

**Location:** `{AGENT_TOOLS_DIR}/symbol_diff.py`

Runs `nm -D --defined-only` on both shared libraries and reports which symbols
are present in the C library but missing from the Rust library (and vice versa).
Rust-internal symbols (`__rust_*`, `_ZN*`) are automatically filtered out.

Use this after building both the C and Rust shared libraries to check that the
Rust `.so` exports every symbol the C `.so` does, including symbols generated
by macros.

**Usage:**

```
python3 {AGENT_TOOLS_DIR}/symbol_diff.py <path-to-c.so> <path-to-rust.so>
```

**Exit codes:**
- `0` — symbol sets match (OK)
- `1` — there are missing or extra symbols (fix needed)

**Example — libraries match:**

```
python3 {AGENT_TOOLS_DIR}/symbol_diff.py \
    c_src/build/libcrypto.so \
    target/release/libcrypto.so
```

Output:
```
OK: exported symbol sets match
```

**Example — missing symbols:**

```
python3 {AGENT_TOOLS_DIR}/symbol_diff.py \
    c_src/build/libsphincs.so \
    target/release/libsphincs.so
```

Output:
```
MISSING in Rust (present in C but not in Rust .so):
  SPHINCS_sign
  SPHINCS_verify
  crypto_sign_BYTES
EXTRA in Rust (not present in C .so):
  sphincs_sign_internal
```

In this case, add `#[unsafe(no_mangle)]` exports for the missing symbols and
remove or rename the extra one.

**Notes:**
- Both `.so` files must already be compiled before calling this tool.
- Requires `nm` (binutils) to be installed; it is available in standard Linux environments.
