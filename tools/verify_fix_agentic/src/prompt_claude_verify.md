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

### Cargo features (FRAMEWORK CONTRACT — also enforced at the final build)
- The build harness invokes `cargo build --features <v1>,<v2>,...` using the
  bare lowercase VALUES of the CMake cache variables for each configuration.
- If `Cargo.toml`'s `[features]` section uses prefixed names (e.g.
  `opt_a_foo = []` with no `foo` alias), the harness will fail. That is a
  Cargo.toml bug, not a test-command bug — fix the Cargo.toml so it exposes
  the bare values (either as primary names or as aliases pointing to the
  internal prefixed gates).
- `[lib] name` and `[[bin]] name` MUST use underscores only — NO hyphens.
  Hyphens cause manifest parse failure. Fix Cargo.toml if you see them.

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
- ## H<N>: <one-line hypothesis>
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


{AGENT_TOOLS_SECTION}

## Static Analysis Tool Wishlist

As you work through verification and fixing, pay attention to moments where you think:
- "If I had a tool that could tell me X, I could skip this lengthy reasoning / exploration."
- "If I had a tool that could do Y, I would have much higher confidence in this fix."

Whenever such a thought arises, **immediately** append one JSON object (on a single line) to
the file `{WISHLIST_PATH}`. Do not wait until the end — record the wish as soon as it occurs,
while the context is fresh. Multiple entries are encouraged; record every distinct need.

Each entry must be a single-line JSON object with exactly these fields:

```
{"category": "...", "description": "...", "language": "...", "soundness": "...", "completeness": "...", "value": 0}
```

Field definitions:
- `category`: `"info_query"` (read-only analysis that answers a question) or `"code_edit"` (a transformation/rewrite tool)
- `description`: plain English description of what the tool does — **no implementation details**, just what it gives you and why it would help
- `language`: `"C"`, `"Rust"`, `"C_and_Rust"`, or another language name
- `soundness`: `"required"` (must never give wrong answers), `"preferred"`, or `"not_needed"` (approximate/heuristic output is fine)
- `completeness`: `"required"` (must cover all cases), `"preferred"`, or `"not_needed"` (partial results are useful enough)
- `value`: integer 0–10 estimating how much this tool would have helped you in this specific task
