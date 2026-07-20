#!/usr/bin/env python3
"""
Slim archived HARVEST run outputs (blocks 1 & 2 of the size-reduction plan).

Block 1 — strip snapshot noise from trace files and output.log:
  OpenCode's workspace-snapshot machinery attaches full text "diff" and
  "patch" strings to every file-edit part in exported sessions. Policy:
  - "diff" fields are always emptied (largely redundant with "patch");
  - "patch" fields are emptied only when they describe a build artifact
    (paths under target/, or .rmeta/.rlib/.so/.o/... — binary noise that
    appears in BOTH fields, so path filtering is still required);
  - "patch" fields for source/text files are KEPT: they are the only
    record of filesystem changes made through bash (not tool inputs), and
    preserve the ability to replay the agent's file operations.
  Emptied values keep the JSON shape valid ("").

Block 2 — keep exactly one copy per run:
  A run directory typically holds both a manually captured trace_<run>.txt
  and output.log with near-identical content. The non-preferred one is
  deleted (default: keep the trace, delete output.log — correct for archives
  produced before the framework made output.log the superset; use
  --prefer output for newer runs).

Safety:
  - Dry-run by default; nothing is modified without --apply.
  - Only touches top-level `trace_*`/`output.log` files of each run dir;
    nested files (e.g. gtest_retest/output.log) are never touched.
  - Block 2 only deletes output.log when a non-empty top-level trace file
    exists (and vice versa with --prefer output).

Usage:
  python3 archive_slim.py harvest-translate-results/agentic            # dry run, all runs
  python3 archive_slim.py harvest-translate-results/agentic/out_x --apply
  python3 archive_slim.py <dir>... --apply --prefer output
"""

import argparse
import json
import os
import re
import sys
import tempfile

# Pretty-printed export JSON: the whole (escaped) string value sits on one line.
_FIELD_LINE_RE = re.compile(r'^(\s*)"(diff|patch)": "((?:[^"\\]|\\.)*)"(,?)\s*$')
# Target-file header at the start of a snapshot diff/patch value.
_INDEX_RE = re.compile(r'^Index: ([^\\\n]+?)(?:\\n|\n|=)')

_ARTIFACT_EXT = (".rmeta", ".rlib", ".so", ".o", ".d", ".rcgu", ".bin",
                 ".timestamp", ".a")


def _is_build_artifact(value: str) -> bool:
    """Whether a diff/patch value describes a build artifact (binary noise)."""
    m = _INDEX_RE.match(value)
    if not m:
        return False
    path = m.group(1)
    return "/target/" in path or path.startswith("target/") \
        or path.endswith(_ARTIFACT_EXT)


def _should_empty(field: str, value: str) -> bool:
    if not value:
        return False
    if field == "diff":
        return True
    return _is_build_artifact(value)


def _strip_singleline_export(line: str) -> str | None:
    """Apply the same policy inside a single-line export block.

    Returns the replacement line, or None if the line is not a single-line
    export block containing such fields.
    """
    s = line.strip()
    if not (s.startswith("{") and '"messages"' in s and ('"diff"' in s or '"patch"' in s)):
        return None
    try:
        obj = json.loads(s)
    except json.JSONDecodeError:
        return None
    if not (isinstance(obj, dict) and "info" in obj and "messages" in obj):
        return None

    changed = [False]

    def walk(o):
        if isinstance(o, dict):
            for k, v in o.items():
                if k in ("diff", "patch") and isinstance(v, str) \
                        and _should_empty(k, v):
                    o[k] = ""
                    changed[0] = True
                else:
                    walk(v)
        elif isinstance(o, list):
            for x in o:
                walk(x)

    walk(obj)
    if not changed[0]:
        return None
    return json.dumps(obj, separators=(",", ":")) + "\n"


def strip_diff_patch(path: str, apply: bool) -> tuple[int, int]:
    """Block 1 on one file. Returns (bytes_before, bytes_saved)."""
    before = os.path.getsize(path)
    saved = 0
    out_fd = None
    out_path = None
    if apply:
        d = os.path.dirname(path) or "."
        fd, out_path = tempfile.mkstemp(prefix=".slim.", dir=d)
        out_fd = os.fdopen(fd, "w", errors="replace")
    try:
        with open(path, "r", errors="replace") as f:
            for line in f:
                m = _FIELD_LINE_RE.match(line)
                if m and _should_empty(m.group(2), m.group(3)):
                    new = f'{m.group(1)}"{m.group(2)}": ""{m.group(4)}\n'
                elif not m and '"messages"' in line \
                        and ('"diff"' in line or '"patch"' in line):
                    new = _strip_singleline_export(line) or line
                else:
                    new = line
                saved += len(line) - len(new)
                if out_fd:
                    out_fd.write(new)
    finally:
        if out_fd:
            out_fd.close()
    if apply and out_path:
        if saved > 0:
            os.replace(out_path, path)
        else:
            os.unlink(out_path)
    return before, max(saved, 0)


def run_dirs(paths: list[str]) -> list[str]:
    """Expand arguments: a run dir itself, or a parent containing out_* dirs."""
    dirs = []
    for p in paths:
        p = p.rstrip("/")
        if not os.path.isdir(p):
            print(f"WARN: not a directory, skipping: {p}", file=sys.stderr)
            continue
        entries = sorted(os.listdir(p))
        subruns = [os.path.join(p, e) for e in entries
                   if e.startswith("out_") and os.path.isdir(os.path.join(p, e))]
        dirs.extend(subruns if subruns else [p])
    return dirs


def top_level_targets(run_dir: str) -> tuple[list[str], str | None]:
    """(trace files, output.log path) at the top level of a run dir."""
    traces = []
    output_log = None
    for e in sorted(os.listdir(run_dir)):
        full = os.path.join(run_dir, e)
        if not os.path.isfile(full):
            continue
        if e == "output.log":
            output_log = full
        elif e.startswith("trace_") and not any(
            t in e for t in ("_readable", "_timeline", "_file_io")
        ):
            traces.append(full)
    return traces, output_log


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="+", help="run dirs, or parents containing out_* dirs")
    ap.add_argument("--apply", action="store_true",
                    help="actually modify files (default: dry run)")
    ap.add_argument("--prefer", choices=("trace", "output"), default="trace",
                    help="which copy to keep in block 2 (default: trace)")
    ap.add_argument("--no-dedup", action="store_true",
                    help="skip block 2 (only strip diff/patch fields)")
    args = ap.parse_args()

    mode = "APPLY" if args.apply else "DRY RUN"
    total_before = total_saved = 0
    for rd in run_dirs(args.paths):
        traces, output_log = top_level_targets(rd)
        candidates = traces + ([output_log] if output_log else [])
        if not candidates:
            continue
        lines = []

        # Block 2 first: don't waste time stripping a file we then delete.
        deleted = set()
        if not args.no_dedup:
            if args.prefer == "trace" and output_log and traces and \
                    any(os.path.getsize(t) > 0 for t in traces):
                lines.append(f"  drop output.log ({os.path.getsize(output_log)/1048576:.1f} MB)"
                             " — trace kept")
                total_saved += os.path.getsize(output_log)
                total_before += os.path.getsize(output_log)
                if args.apply:
                    os.unlink(output_log)
                deleted.add(output_log)
            elif args.prefer == "output" and output_log and \
                    os.path.getsize(output_log) > 0:
                for t in traces:
                    lines.append(f"  drop {os.path.basename(t)} "
                                 f"({os.path.getsize(t)/1048576:.1f} MB) — output.log kept")
                    total_saved += os.path.getsize(t)
                    total_before += os.path.getsize(t)
                    if args.apply:
                        os.unlink(t)
                    deleted.add(t)

        # Block 1 on whatever survives.
        for f in candidates:
            if f in deleted:
                continue
            before, saved = strip_diff_patch(f, args.apply)
            total_before += before
            total_saved += saved
            if saved:
                lines.append(f"  strip {os.path.basename(f)}: "
                             f"{before/1048576:.1f} → {(before-saved)/1048576:.1f} MB "
                             f"(-{100*saved/before:.0f}%)")

        if lines:
            print(f"[{mode}] {rd}")
            for line in lines:
                print(line)

    print(f"\n[{mode}] total: {total_before/1048576:.1f} MB scanned, "
          f"{total_saved/1048576:.1f} MB removable"
          f"{'' if args.apply else ' (re-run with --apply to modify)'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
