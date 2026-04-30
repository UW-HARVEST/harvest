### Tool: `unifdef` — Resolve C preprocessor conditionals

**System tool** (available as `unifdef` on `PATH` — no script path needed).

Statically removes `#ifdef` / `#ifndef` / `#if defined()` / `#elif` / `#endif`
blocks from C source files given a set of defined and undefined macros. Output
is the source with the specified conditionals resolved, leaving unrelated code
untouched.

Also handles value comparisons: `-DFOO=16` correctly resolves `#if FOO == 16`,
`#if FOO >= 8`, etc.

---

#### Basic usage

```
unifdef [-Dsym[=val] ...] [-Usym ...] [file ...]
```

- `-Dsym` — define `sym` as 1
- `-Dsym=val` — define `sym` with a specific integer value
- `-Usym` — treat `sym` as undefined (remove the `#ifdef sym` branch)
- Output goes to **stdout** by default; use `-opath` to write to a file
- `-B` — compress extra blank lines left after deleted sections (cleaner output)

**Exit codes:** `0` = file unchanged, `1` = lines were removed/changed, `2` = error.
Exit code `1` is normal and expected when conditionals are resolved.

---

#### Examples

**Resolve a single feature flag:**
```
unifdef -DSPX_SHA2 -USPX_SHAKE -USPX_HARAKA hash.c
```

**Resolve with a value-defined macro:**
```
unifdef -DSPX_WOTS_W=16 -B params.h
```

**Write result to a file instead of stdout:**
```
unifdef -DBLAKE_TR -USPX_N -B \
    c_src/app/src/PQCgenKAT_sign.c \
    -o /tmp/PQCgenKAT_sign_resolved.c
```

**Resolve multiple defines at once:**
```
unifdef -DSPX_SHA2 -DSPX_N=32 -DSPX_WOTS_W=16 -USPX_HARAKA -B \
    c_src/app/src/hash_sha2.c
```

**List all `#if` control symbols in a file (useful before deciding what to define):**
```
unifdef -s c_src/app/src/thash_sha2_simple.c
```

---

#### What unifdef handles vs. does NOT handle

| Handled | Not handled |
|---------|-------------|
| `#ifdef FOO` | Macro body expansion (`#define X Y` → X substitution) |
| `#ifndef FOO` | `#include` directives (passed through unchanged) |
| `#if defined(FOO)` | Complex boolean with undefined symbols (`#if A && B` when B unknown) |
| `#if FOO == val` (with `-DFOO=val`) | |
| `#elif` chains | |

Conditionals that cannot be resolved (because the controlling symbol was neither
`-D` nor `-U`) are left in the output verbatim.

---

#### Workflow tip

Use `unifdef -s <file>` first to see which symbols control the conditionals, then
pass the appropriate `-D`/`-U` flags to resolve the ones relevant to the variant
you are translating.
