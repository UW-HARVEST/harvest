#!/usr/bin/env python3
"""
c_sandbox — compile and run a C snippet, return its exact stdout.

Read a complete C program from stdin (or a file argument), compile it with gcc,
run it, and print its stdout. Exit 1 on compile or runtime error.
"""

import sys
import os
import tempfile
import subprocess


def main():
    if len(sys.argv) > 1 and sys.argv[1] in ("--help", "-h"):
        print(__doc__.strip())
        return

    if len(sys.argv) > 1 and sys.argv[1] != "-":
        with open(sys.argv[1]) as f:
            code = f.read()
    else:
        code = sys.stdin.read()

    with tempfile.TemporaryDirectory() as tmpdir:
        src = os.path.join(tmpdir, "snippet.c")
        exe = os.path.join(tmpdir, "snippet")

        with open(src, "w") as f:
            f.write(code)

        compile_result = subprocess.run(
            ["gcc", "-O0", "-o", exe, src, "-lm"],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode != 0:
            print("COMPILE ERROR:", file=sys.stderr)
            print(compile_result.stderr, file=sys.stderr)
            sys.exit(1)

        try:
            run_result = subprocess.run(
                [exe],
                capture_output=True,
                text=True,
                timeout=10,
            )
        except subprocess.TimeoutExpired:
            print("TIMEOUT: program ran longer than 10 seconds", file=sys.stderr)
            sys.exit(1)

        sys.stdout.write(run_result.stdout)
        if run_result.stderr:
            sys.stderr.write(run_result.stderr)
        if run_result.returncode != 0:
            print(f"RUNTIME EXIT: {run_result.returncode}", file=sys.stderr)
            sys.exit(1)


if __name__ == "__main__":
    main()
