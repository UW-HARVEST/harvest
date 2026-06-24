#!/usr/bin/env python3
"""visualize.py — Run parse_trace.py -v on all trace_* files in current directory."""

import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "parse_trace.py"
if not SCRIPT.exists():
    print(f"ERROR: {SCRIPT} not found", file=sys.stderr)
    sys.exit(1)

traces = sorted(Path().glob("trace_*"))
if not traces:
    print("No trace_* files found in current directory.")
    sys.exit(0)

for f in traces:
    if f.suffix == ".svg":
        continue  # skip already-generated SVGs
    if f.suffix == ".json":
        continue  # skip already-generated JSONs
    # Skip derived files (_readable.txt, _file_io.json, etc.)
    if "_readable" in f.stem or "_file_io" in f.stem:
        continue
    svg = f.parent / (f.stem + "_timeline.svg")
    if svg.exists() and svg.stat().st_mtime >= f.stat().st_mtime:
        # print(f"  {f.name} -> {svg.name}  (up to date, skip)")
        continue
    print(f"  {f.name} ... ", end="", flush=True)
    r = subprocess.run(
        ["python3", str(SCRIPT), str(f), "-v"],
        capture_output=True, text=True, timeout=300,
    )
    if r.returncode == 0:
        out = f.parent / (f.stem + "_timeline.svg")
        print(out.name)
    else:
        print(f"FAILED")
        if r.stderr:
            print(f"    {r.stderr.strip()[:120]}", file=sys.stderr)
