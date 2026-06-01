<!-- markdownlint-disable MD041 -->
You are testing a C-to-Rust translation for correctness. The C code is the
ground truth — the Rust code must produce byte-identical results.

- `c_src/` contains the original C source code
- `src/` contains the Rust translation
- The C code can be compiled as a shared library. Look at c_src/CMakeLists.txt
  to understand the build system. Build it with:
  ```
  cd c_src && mkdir -p build && cd build && \
  cmake .. -DCMAKE_POSITION_INDEPENDENT_CODE=ON {CMAKE_BUILD_FLAGS} && \
  cmake --build .
  ```
- Find the resulting .so files in the build output

## Step 0: Read PLAN.md FIRST

A previous translation agent left a file `PLAN.md` in this directory containing
its design notes, parameter tables, decisions, and pitfalls it noticed during
translation. **Before doing anything else**, run:

```
cat PLAN.md
```

If `PLAN.md` exists, treat it as authoritative background. Do NOT re-derive
project structure, module layout, Cargo features, parameter values, or
design rationale from scratch — that information is already there. Pay
particular attention to the "Notes for future-me" section the translation
agent may have flagged specific concerns (e.g. "macro renames", "padding
edge cases") that point directly at likely bug sites.

If `PLAN.md` does not exist, the project was small enough that the translator
chose not to write one; proceed without it.

## Step 1: Maintain `HYPOTHESES.md`

Verification work involves forming hypotheses about why the C and Rust outputs
differ, then confirming or refuting them. Your context window is finite and
will be **compacted** when it fills up; after a compaction your memory of which
hypotheses you already investigated is **lost**, leading to "rediscover the
same bug three times" loops that waste an entire run.

To prevent this, you MUST maintain a file `HYPOTHESES.md` in the current
directory as an append-only log of bug hypotheses. Create it at the very
start of your work (right after reading `PLAN.md`) with this template:

```markdown
# Verification Hypotheses Log

This is an append-only log of bug hypotheses I form while verifying the
Rust translation. After every compaction I will `cat HYPOTHESES.md` first
thing to recover state.

## Invariants (do not drift across compactions)

These rules govern verification. They must survive every compaction unchanged.

### AFTER ANY COMPACTION: `cat PLAN.md HYPOTHESES.md` is your FIRST action before anything.

### Ground truth
- The C code is the authoritative reference. Rust outputs must match C
  byte-for-byte (binary stdout AND every public function output under
  libloading-based comparison).
- If C and Rust diverge, fix Rust. NEVER modify C.

### Cargo features (FRAMEWORK CONTRACT)
- The `Cargo.toml`, `build.rs`, and `rust-toolchain.toml` are a provided
  scaffold; do NOT modify them. The `[features]` block exposes one feature per
  configurable-variable value, named `VAR_value` (e.g. `HASH_BACKEND_blake`).
- The harness rebuilds each configuration with
  `cargo build --no-default-features --features <VAR_value>,...` using those
  exact names; Rust code gates on the bare cfg `#[cfg(VAR_value)]`.
- If a configuration fails to build or diverges, fix the Rust sources under
  `src/`, NOT the provided `Cargo.toml`/`build.rs`.

### Boundaries
- Do NOT modify anything in `c_src/`.
- Add `libloading = "0.8"` to `[dev-dependencies]` in `Cargo.toml` (so your
  integration tests can dlopen the C shared library).

### Configuration coverage
- Every configuration listed under "Configurations to verify" in this task
  must be checked. For each one:
    1. Clean and rebuild C with the listed cmake flags.
    2. Rebuild Rust with the matching Cargo features
       (`cargo build --release --no-default-features --features <list>`).
    3. Re-run integration tests and fix any mismatches before moving on.

{ALL_CONFIGURATIONS}

### Operational
- Wrap every `cargo build`, `cargo test`, `cmake`, or other long-running
  command in `timeout 600` (or shorter). No single command should run > 600s.
- If a single test takes too long, skip it and move on. Do not get stuck on
  one step.

## Hypothesis log

Format per entry:
### H<N>: <one-line hypothesis>
- Status: open | confirmed | refuted | fixed
- Evidence: <how I think I know>
- Files/lines suspected: <file path:line>
- Action taken: <Edit/test/none yet>
- Outcome: <what happened after the action>
```

**Rules of engagement with `HYPOTHESES.md`:**

1. **The `## Invariants` section is verbatim.** When you create
   `HYPOTHESES.md`, copy the entire `## Invariants` block from the template
   above byte-for-byte. Do not paraphrase, do not omit, do not reorder. The
   hypothesis log section you fill in as you work; Invariants is fixed text.
   Reason: anything outside HYPOTHESES.md drifts after compaction; only this
   file reliably comes back. Invariants must be byte-stable.
2. **Every time you form a new hypothesis** (e.g. "I think `foo()` has an
   off-by-one in the padding length"), append a new `## H<N>` entry
   immediately, with status `open`. Do NOT wait until you have proof.
3. **After running a test that bears on a hypothesis**, update its `Status`
   to `confirmed` or `refuted` and write the evidence in `Outcome`. Do NOT
   leave entries stale.
4. **After applying an Edit that you believe fixes a hypothesis**, mark it
   `fixed` and note what you changed.
5. **Before forming a new hypothesis, check if it is already in the file.**
   If so, do not re-investigate it — read its current Status and proceed.
6. **After every compaction, `cat HYPOTHESES.md` first thing.** Re-read the
   `## Invariants` section, then the hypothesis log. If the very first
   hypothesis you form already exists with status `confirmed` or `fixed`,
   you are in a thrashing loop — stop, re-read the existing entry, and
   continue from where it left off (e.g. if status is `confirmed` but not
   yet `fixed`, your next action is to apply the fix, not re-confirm).
7. The file is for **future-you across your own compactions**, not for
   sub-agents. Do not delegate; you maintain it yourself.
8. **Delegate fixing work aggressively. Your context window is the
   bottleneck — protect it.** Your job as the main agent is to OWN
   HYPOTHESES.md and OWN execution: building C and Rust, running tests,
   running `nm`, comparing C-vs-Rust outputs, deciding which functions
   diverge. Almost everything else — reading large C source files to
   understand an algorithm, locating the matching Rust code, applying
   the actual fix — should go to a sub-agent so neither the C nor the
   buggy Rust ever has to live in YOUR context. Default to delegating;
   only do a fix in-process when it is a one-line change you can apply
   from what you already see.

   Things you keep:
   - HYPOTHESES.md ownership (sub-agents do NOT edit HYPOTHESES.md)
   - Building C / Rust, running cargo test, running nm, output comparison
   - Hypothesis status updates after each test run
   - Per-configuration coverage tracking

   Rule of thumb: if investigating or fixing a hypothesis would require
   reading more than ~200 lines of C or Rust into your own context,
   delegate the fix to a sub-agent and let it report back what it changed.

### Recovery protocol (if you suspect you were just compacted)

Symptoms: you cannot recall what hypothesis you were testing, or your last
turn looks like a summary rather than concrete work. In that case:

1. `cat PLAN.md HYPOTHESES.md` first thing.
2. Find the first hypothesis with status `open` or `confirmed` (but not
   yet `fixed`). That is your current work item.
3. Resume from its `Action taken` field. Do not redo work already logged.

## Step 2: Verification workflow

Now do the actual verification:

1. Build the C code as a shared library
2. Write Rust integration tests (in tests/) that use `libloading`
   to load the C .so and compare C vs Rust function outputs
3. Start with the lowest-level functions and work upward to higher-level ones.
   Look at the C headers to identify the public API and function call hierarchy.
4. For each function: create fixed test inputs, call both C and Rust versions,
   assert outputs match byte-for-byte
5. Run `cargo test` and investigate any mismatches. Every time a test
   exposes a divergence, append a hypothesis to `HYPOTHESES.md`.
6. When you find a Rust function that produces different output than C,
   fix the Rust code in src/ and re-run until the test passes. Update the
   matching hypothesis to `fixed` after the Edit.
7. Keep going until all public functions match
8. If the project has a main binary, run both the C binary and the Rust binary
   with the same inputs and compare their stdout byte-for-byte. Fix any differences.
9. Compare `nm -D` on the C .so and the Rust .so. Every symbol the C .so
   exports, the Rust .so must also export with the exact same name. This
   includes symbols created by preprocessor macros. If the C .so exports it,
   the Rust .so must export it — no exceptions. Add missing exports.

All operational rules (libloading dev-dep, c_src boundary, per-configuration
re-verification, the 600-second timeout cap) live in the `## Invariants`
section of your `HYPOTHESES.md` template above. Re-read them from
`HYPOTHESES.md` whenever you are unsure — do not work from memory of this
prompt.
## Waiting on long-running commands

Building the C reference, building Rust, and running KAT/signing tests can be
slow (some configurations take minutes). When you need to wait for a long
command, run it with `run_in_background` and poll for completion, or wrap a
short sleep in a condition loop (e.g. `until [ -f done.marker ]; do sleep 2; done`).
Do NOT block on a single long foreground `sleep` such as `sleep 30 && cat log` --
it will be rejected, and chaining `sleep` calls only wastes turns.
