### Tool: `symscan` — Dynamic symbol analysis for C/Rust shared libraries

**Location:** `{AGENT_TOOLS_DIR}/symscan.py`

Analyzes dynamic symbol tables using `nm` and `ldd`. Three subcommands cover
the most common symbol-related problems in C-to-Rust translation.

---

#### `diff` — Compare exported symbol sets

Reports which symbols are present in the C `.so` but missing from the Rust `.so`,
and vice versa (after filtering out Rust-internal symbols).

```
python3 {AGENT_TOOLS_DIR}/symscan.py diff <c.so> <rust.so>
```

**Example:**
```
python3 {AGENT_TOOLS_DIR}/symscan.py diff \
    c_src/build/libsphincs.so \
    target/release/libsphincs.so
```

Output when symbols are missing:
```
MISSING in Rust (present in C but not in Rust .so):
  SPHINCS_sign
  crypto_sign_BYTES
EXTRA in Rust (not present in C .so):
  sphincs_sign_internal
```

Output when everything matches:
```
OK: exported symbol sets match
```

Exit code: `0` if sets match, `1` if there are differences.

---

#### `imports` — List symbols imported from outside

Lists all **undefined** symbols in a `.so` — symbols it expects to receive from
the caller or a preloaded library. Classifies each as `system` (provided by a
linked system library such as `libc`) or `application` (must be supplied by the
caller, e.g. via `--rdynamic` or `RTLD_GLOBAL` preload).

```
python3 {AGENT_TOOLS_DIR}/symscan.py imports <file.so>
```

**Example:**
```
python3 {AGENT_TOOLS_DIR}/symscan.py imports c_src/build/libsphincs.so
```

Output:
```
System symbols (12, from linked system libraries):
  memcpy
  memset
  ...

Application symbols (3, must come from caller or preload):
  sha256_inc_blocks
  sha512_inc_blocks
  haraka_S
```

Use this to understand what the test harness must export (via `--rdynamic` or
a `RTLD_GLOBAL` preloaded library) for `dlopen` to succeed.

---

#### `interposition` — Detect symbol interposition risks

When a Rust test binary uses `dlopen` to load a C `.so`, symbols defined in
**both** the test binary and the `.so` are interposition risks: the dynamic
linker may redirect the `.so`'s own internal calls to the test binary's
version, causing infinite recursion or silent wrong behavior.

```
python3 {AGENT_TOOLS_DIR}/symscan.py interposition <test_binary> <lib.so>
```

**Example:**
```
python3 {AGENT_TOOLS_DIR}/symscan.py interposition \
    target/debug/my_test_binary \
    c_src/build/libsphincs.so
```

Output when conflicts exist:
```
INTERPOSITION RISK: 2 symbol(s) defined in both binaries.
When the test binary dlopen()s the library without RTLD_DEEPBIND,
the library's own calls to these symbols will resolve to the test
binary's version, likely causing infinite recursion.

Conflicting symbols:
  sha256_inc_blocks
  shake256

Fix: use RTLD_DEEPBIND in dlopen(), or rename the test binary's exports.
```

Output when safe:
```
OK: no interposition conflicts found
```

Exit code: `0` if no conflicts, `1` if conflicts found.

---

**Notes:**
- Requires `nm` and `ldd` (standard on Linux).
- All three subcommands must be run after the relevant binaries are compiled.
- For `imports`, classification relies on `ldd` to find linked system libraries;
  if the binary is not yet linked, run `diff` on the headers instead.
