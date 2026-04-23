#!/usr/bin/env python3
"""
symbol_diff — compare exported symbols between a C .so and a Rust .so.

Usage: symbol_diff.py <c.so> <rust.so>

Runs `nm -D --defined-only` on both files, computes the symmetric difference,
and reports which symbols are missing from the Rust side. Rust-internal symbols
(__rust_*, _ZN*) are filtered out. Exits 0 if the sets match, 1 otherwise.
"""

import sys
import subprocess


RUST_INTERNAL_PREFIXES = ("__rust_", "_ZN", "rust_eh_", "__rdl_")


def get_exported_symbols(so_path: str) -> set[str]:
    result = subprocess.run(
        ["nm", "-D", "--defined-only", so_path],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"ERROR running nm on {so_path}:", file=sys.stderr)
        print(result.stderr, file=sys.stderr)
        sys.exit(1)

    symbols = set()
    for line in result.stdout.splitlines():
        parts = line.split()
        if len(parts) < 3:
            continue
        name = parts[-1]
        if not any(name.startswith(p) for p in RUST_INTERNAL_PREFIXES):
            symbols.add(name)
    return symbols


def main():
    if len(sys.argv) != 3 or sys.argv[1] in ("--help", "-h"):
        print(__doc__.strip())
        sys.exit(0 if "--help" in sys.argv or "-h" in sys.argv else 1)

    c_so, rust_so = sys.argv[1], sys.argv[2]
    c_syms = get_exported_symbols(c_so)
    rust_syms = get_exported_symbols(rust_so)

    missing = sorted(c_syms - rust_syms)
    extra = sorted(rust_syms - c_syms)

    if not missing and not extra:
        print("OK: exported symbol sets match")
        sys.exit(0)

    if missing:
        print("MISSING in Rust (present in C but not in Rust .so):")
        for s in missing:
            print(f"  {s}")
    if extra:
        print("EXTRA in Rust (not present in C .so):")
        for s in extra:
            print(f"  {s}")

    sys.exit(1)


if __name__ == "__main__":
    main()
