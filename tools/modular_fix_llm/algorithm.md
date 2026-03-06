# ModularFixLlm Algorithm

## Overview

ModularFixLlm is a post-translation repair pass that takes a Rust package produced by an earlier pipeline stage, attempts to compile it, and iteratively asks an LLM to fix any compilation errors declaration by declaration, until the package builds cleanly or a configured iteration limit is reached.

Each declaration is analyzed independently.  No changes to declaration signatures are permitted.  (This limitation should be lifted in the future.)

The tool is not specific to modular translation. It operates on any `CargoPackage` from the IR.

## Input and Output

Because this is a pipeline pass, its input and output have the same format:  a `CargoPackage` in the pipeline IR, containing `Cargo.toml` and either `src/main.rs` or `src/lib.rs`.

The output is either the same `CargoPackage` with its source replaced by a version that compiles, or the last attempted state if the iteration limit is reached without success.

- Configuration (under `[tools.modular_fix_llm]` in the pipeline config):
    - `max_iterations`: maximum number of repair attempts before giving up (default 5)
    - LLM settings forwarded to the underlying `HarvestLLM` client

## Pseudocode

```
function ModularFixLlm(pkg: CargoPackage) -> CargoPackage:
    // Initialization
    (source_file: String, cargo_toml: String) = extract(pkg)   // src/main.rs or src/lib.rs

    // One Rust source snippet per top-level item (fn, struct, impl, type alias, const, ...)
    // Re-formatting is needed to ensure a consistent line index mapping,
    // which is used to attribute compiler errors to declarations.
    declarations: [String] = split_and_format(source_file)

    // Span: location of one declaration in the assembled source, consisting of:
    // - start, end: 1-indexed absolute line numbers in join(declarations, "\n\n"),
    //               accounting for the blank separator lines between declarations
    // - decl_idx:   index into declarations[]
    line_index: [Span] = build_line_index(declarations)

    // Repair loop
    for iter = 0 .. max_iterations:
        source: String = join(declarations, "\n\n")
        save_snapshot(iter, source) // <history_dir>/iter_<N>/

        // cargo build --release --message-format=json
        compilation_result = cargo_build(source, cargo_toml)

        if compilation_result.success:
            return assemble(declarations, cargo_toml)

        // parse all compiler diagnostics; both errors and warnings are returned
        all_diagnostics: [Diagnostic] = parse_diagnostics(compilation_result.output)

        // Only error-level diagnostics would drive fixes,
        // but we collect warnings too to provide more context to the LLM
        decl_errors: Map<int, [String]> = {}
        decl_warnings: Map<int, [String]> = {}
        for diag in all_diagnostics:
            decl_idx = line_index.find_if(span => diag.line in span.start .. span.end)?.decl_idx
            if diag.level == "error":
                decl_errors[decl_idx].append(diag.rendered_text)
            elif diag.level == "warning":
                decl_warnings[decl_idx].append(diag.rendered_text)

        // replace every fn body with { todo!() }; computed once per iteration for prefix caching
        interface_ctx: String = stub_all_bodies(declarations)

        for (decl_idx, errs) in decl_errors:
            declarations[decl_idx] = llm_fix(
                context  = interface_ctx,
                errors   = format(errs),
                // Warnings are fixed only if they accompany errors in the same declaration
                warnings = format(decl_warnings.get_or_else(decl_idx, [])),
                decl     = declarations[decl_idx]
            )

    return assemble(declarations, cargo_toml)
```
