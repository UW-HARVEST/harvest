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
    (source_file, cargo_toml) = extract(pkg)        // src/main.rs or src/lib.rs + Cargo.toml
    declarations = split_and_format(source_file)    // syn parse + prettyplease reformat per item
    line_index = build_line_index(declarations)      // (start, end, decl_idx)

    // Repair loop
    for iter = 0 .. max_iterations:
        source = join(declarations, "\n\n")
        save_snapshot(iter, source)                  // <history_dir>/iter_<N>/

        compilation_result = cargo_build(source, cargo_toml)     // cargo build --release --message-format=json

        if compilation_result.success:
            return assemble(declarations, cargo_toml)

        errors = classify(compilation_result.output) // keep error-level diagnostics only
                                                     // warnings are included in error text for LLM context
                                                     // but do not lead to repair attempts

        decl_errors = {}
        for error in errors:
            decl_idx = line_index.find(error.primary_span.line)
            decl_errors[decl_idx].append(error.rendered_text)

        interface_ctx = stub_all_bodies(declarations)   // replace fn bodies with { todo!() };
                                                        // computed once per iteration for prefix caching

        for (decl_idx, errs) in decl_errors:
            fixed_source = llm_fix(
                context = interface_ctx,
                errors  = format(errs),
                decl    = declarations[decl_idx].source
            )
            declarations[decl_idx].source = fixed_source

    return assemble(declarations, cargo_toml)
```
