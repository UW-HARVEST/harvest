#!/usr/bin/env python3
"""
symscan — dynamic symbol analysis for C/Rust shared libraries.

Subcommands:
  diff <c.so> <rust.so>
      Compare exported symbol sets.
  imports <file.so>
      List symbols imported from outside; classify system vs. application.
  interposition <test_bin> <lib.so>
      Detect symbols exported by both binaries (interposition risk).
"""

import sys
import os
import subprocess


RUST_INTERNAL_PREFIXES = ("__rust_", "_ZN", "rust_eh_", "__rdl_")


# ---------------------------------------------------------------------------
# nm helpers
# ---------------------------------------------------------------------------

def _nm_defined(path: str) -> set[str]:
    r = subprocess.run(["nm", "-D", "--defined-only", path],
                       capture_output=True, text=True)
    if r.returncode != 0:
        _die(f"nm failed on {path}:\n{r.stderr}")
    out = set()
    for line in r.stdout.splitlines():
        parts = line.split()
        if len(parts) >= 3:
            name = parts[-1]
            if not any(name.startswith(p) for p in RUST_INTERNAL_PREFIXES):
                out.add(name)
    return out


def _nm_undefined(path: str) -> set[str]:
    r = subprocess.run(["nm", "-D", "--undefined-only", path],
                       capture_output=True, text=True)
    if r.returncode != 0:
        _die(f"nm failed on {path}:\n{r.stderr}")
    out = set()
    for line in r.stdout.splitlines():
        parts = line.split()
        if parts:
            out.add(parts[-1])
    return out


def _ldd_libs(path: str) -> list[str]:
    """Return absolute paths of shared libraries linked by path."""
    r = subprocess.run(["ldd", path], capture_output=True, text=True)
    libs = []
    for line in r.stdout.splitlines():
        if "=>" not in line:
            continue
        rhs = line.split("=>", 1)[1].strip()
        lib_path = rhs.split()[0] if rhs.split() else ""
        if lib_path and os.path.isfile(lib_path):
            libs.append(lib_path)
    return libs


def _system_symbols(path: str) -> set[str]:
    """
    Collect exported symbols from all system libraries linked by path.
    A library is considered 'system' if it lives under /lib or /usr/lib.
    """
    sys_dirs = ("/lib", "/usr/lib", "/lib64", "/usr/lib64")
    syms: set[str] = set()
    for lib in _ldd_libs(path):
        if not any(lib.startswith(d) for d in sys_dirs):
            continue
        try:
            r = subprocess.run(["nm", "-D", "--defined-only", lib],
                               capture_output=True, text=True, timeout=10)
            for line in r.stdout.splitlines():
                parts = line.split()
                if len(parts) >= 3:
                    syms.add(parts[-1])
        except subprocess.TimeoutExpired:
            pass
    return syms


def _die(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------

def cmd_diff(c_so: str, rust_so: str) -> int:
    """Compare exported symbol sets between a C .so and a Rust .so."""
    c_syms = _nm_defined(c_so)
    rust_syms = _nm_defined(rust_so)

    missing = sorted(c_syms - rust_syms)
    extra = sorted(rust_syms - c_syms)

    if not missing and not extra:
        print("OK: exported symbol sets match")
        return 0

    if missing:
        print("MISSING in Rust (present in C but not in Rust .so):")
        for s in missing:
            print(f"  {s}")
    if extra:
        print("EXTRA in Rust (not present in C .so):")
        for s in extra:
            print(f"  {s}")
    return 1


def cmd_imports(path: str) -> int:
    """List symbols imported by path; classify as system or application."""
    undef = _nm_undefined(path)
    if not undef:
        print("No undefined symbols found.")
        return 0

    sys_syms = _system_symbols(path)
    system = sorted(s for s in undef if s in sys_syms)
    application = sorted(s for s in undef if s not in sys_syms)

    if system:
        print(f"System symbols ({len(system)}, from linked system libraries):")
        for s in system:
            print(f"  {s}")
    if application:
        print(f"\nApplication symbols ({len(application)}, must come from caller or preload):")
        for s in application:
            print(f"  {s}")

    return 0


def cmd_interposition(test_bin: str, lib_so: str) -> int:
    """
    Find symbols exported by both test_bin and lib_so.
    These are interposition risks: when test_bin dlopen()s lib_so,
    the dynamic linker may redirect lib_so's internal calls to
    test_bin's definitions, causing infinite recursion.
    """
    bin_syms = _nm_defined(test_bin)
    lib_syms = _nm_defined(lib_so)

    conflicts = sorted(bin_syms & lib_syms)

    if not conflicts:
        print("OK: no interposition conflicts found")
        return 0

    print(f"INTERPOSITION RISK: {len(conflicts)} symbol(s) defined in both binaries.")
    print("When the test binary dlopen()s the library without RTLD_DEEPBIND,")
    print("the library's own calls to these symbols will resolve to the test")
    print("binary's version, likely causing infinite recursion.\n")
    print("Conflicting symbols:")
    for s in conflicts:
        print(f"  {s}")
    print("\nFix: use RTLD_DEEPBIND in dlopen(), or rename the test binary's exports.")
    return 1


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

USAGE = """\
Usage:
  symscan.py diff <c.so> <rust.so>
  symscan.py imports <file.so>
  symscan.py interposition <test_bin> <lib.so>
  symscan.py --help
"""

def main() -> None:
    args = sys.argv[1:]
    if not args or args[0] in ("--help", "-h"):
        print(__doc__.strip())
        print()
        print(USAGE)
        return

    cmd = args[0]
    rest = args[1:]

    if cmd == "diff":
        if len(rest) != 2:
            _die("diff requires exactly two arguments: <c.so> <rust.so>")
        sys.exit(cmd_diff(rest[0], rest[1]))

    elif cmd == "imports":
        if len(rest) != 1:
            _die("imports requires exactly one argument: <file.so>")
        sys.exit(cmd_imports(rest[0]))

    elif cmd == "interposition":
        if len(rest) != 2:
            _die("interposition requires exactly two arguments: <test_bin> <lib.so>")
        sys.exit(cmd_interposition(rest[0], rest[1]))

    else:
        _die(f"unknown subcommand '{cmd}'. Run with --help for usage.")


if __name__ == "__main__":
    main()
