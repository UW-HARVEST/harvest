#!/usr/bin/env python3
"""
callgraph — static C call graph from compile_commands.json

Uses clang (LLVM IR) + llvm-link + opt to build a whole-program call graph,
then emits it in human-readable text.

Subcommands:
  list  <compile_commands.json>
      Print every function and its direct callees, sorted alphabetically.
  from  <compile_commands.json> <function> [--depth N]
      Print the call tree rooted at <function>.  --depth limits traversal.
  dot   <compile_commands.json>
      Emit the raw DOT source (for graphviz rendering).

Limitations:
  - Every source file must compile (all headers reachable, all -D flags
    present in compile_commands.json).  Files that fail are skipped with a
    warning; the graph covers only what compiled successfully.
  - Function pointers are NOT resolved.  A function called only via a
    pointer appears as a leaf with no outgoing edges.
"""

import sys
import os
import re
import json
import shlex
import subprocess
import tempfile
import shutil
from pathlib import Path


LLVM_INTRINSIC_PREFIX = "llvm."
# Compiler builtins and libc wrappers that clutter the graph
_SKIP_PREFIXES = ("llvm.", "__builtin_", "__bswap_", "__uint", "__int")


# ---------------------------------------------------------------------------
# Compilation helpers
# ---------------------------------------------------------------------------

def _strip_compile_flags(args: list[str]) -> list[str]:
    """Remove flags that conflict with -emit-llvm / -S output mode."""
    skip_next = False
    result = []
    for arg in args:
        if skip_next:
            skip_next = False
            continue
        if arg in ("-c", "-MMD", "-MP", "-pipe"):
            continue
        if arg in ("-o", "-MF", "-MT", "-MQ", "--serialize-diagnostics"):
            skip_next = True
            continue
        result.append(arg)
    return result


def _compile_to_ir(entry: dict, out_dir: str) -> str | None:
    """
    Compile one compile_commands entry to LLVM IR (.ll).
    Returns the path to the .ll file, or None if compilation failed.
    """
    src = entry["file"]
    directory = entry.get("directory", ".")

    if "arguments" in entry:
        args = list(entry["arguments"])
    else:
        args = shlex.split(entry["command"])

    # Replace whatever compiler was used with clang, strip output flags.
    args[0] = "clang"
    args = _strip_compile_flags(args)

    # Build a collision-free output name.
    unique = abs(hash(src)) % 10 ** 9
    out_ll = str(Path(out_dir) / f"{Path(src).stem}_{unique}.ll")

    args += ["-S", "-emit-llvm", "-O0", "-o", out_ll]

    r = subprocess.run(args, capture_output=True, text=True, cwd=directory)
    if r.returncode != 0:
        short = Path(src).name
        print(f"  SKIP {short}: compilation failed", file=sys.stderr)
        for line in r.stderr.splitlines()[:3]:
            print(f"    {line}", file=sys.stderr)
        return None
    return out_ll


def _build_graph(compile_commands_path: str) -> dict[str, set[str]]:
    """
    Full pipeline: compile → link → dot-callgraph → parse.
    Returns a dict mapping each function name to the set of functions it calls.
    """
    entries = json.loads(Path(compile_commands_path).read_text())

    # Deduplicate entries by source file path: the same .c file may appear
    # multiple times (once per target) with identical IR content.
    seen_src: set[str] = set()
    deduped: list[dict] = []
    for entry in entries:
        src = entry["file"]
        if src not in seen_src:
            seen_src.add(src)
            deduped.append(entry)

    tmp = tempfile.mkdtemp(prefix="callgraph_")
    try:
        # Step 1: compile each file to LLVM IR
        n = len(deduped)
        print(f"Compiling {n} file(s) to LLVM IR …", file=sys.stderr)
        ll_files = []
        for entry in deduped:
            ll = _compile_to_ir(entry, tmp)
            if ll:
                ll_files.append(ll)

        if not ll_files:
            _die("No files compiled successfully — cannot build call graph.")

        print(f"  {len(ll_files)}/{n} file(s) compiled OK", file=sys.stderr)

        # Step 2: link into a single bitcode module.
        # Use --override for every file after the first so that duplicate
        # symbol definitions (e.g. two alternative implementations of the
        # same function compiled for different build targets) do not cause a
        # hard error — the last definition wins.
        bc = os.path.join(tmp, "merged.bc")
        link_args = ["llvm-link", ll_files[0]]
        for ll in ll_files[1:]:
            link_args += ["--override", ll]
        link_args += ["-o", bc]
        r = subprocess.run(link_args, capture_output=True, text=True)
        if r.returncode != 0:
            _die(f"llvm-link failed:\n{r.stderr}")

        # Step 3: generate DOT call graph
        # opt writes <input-basename>.callgraph.dot in the CWD.
        r = subprocess.run(
            ["opt", "-passes=dot-callgraph", bc, "-o", "/dev/null"],
            capture_output=True, text=True, cwd=tmp,
        )
        # opt exits 0 even on success but warns on stderr — ignore stderr here.

        dot_path = Path(tmp) / "merged.bc.callgraph.dot"
        if not dot_path.exists():
            _die("opt did not produce merged.bc.callgraph.dot")

        dot_content = dot_path.read_text()
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    return _parse_dot(dot_content)


# ---------------------------------------------------------------------------
# DOT parser
# ---------------------------------------------------------------------------

def _parse_dot(dot: str) -> dict[str, set[str]]:
    node_name: dict[str, str] = {}   # node-id  →  function name
    raw_edges: list[tuple[str, str]] = []

    for line in dot.splitlines():
        line = line.strip()
        # Node definition:  NodeXXX [shape=record,label="{func_name}"];
        m = re.match(r"(Node\w+)\s+\[.*?label=\"\{([^}]+)\}\"", line)
        if m:
            node_name[m.group(1)] = m.group(2)
            continue
        # Edge:  NodeXXX -> NodeYYY;
        m = re.match(r"(Node\w+)\s*->\s*(Node\w+)", line)
        if m:
            raw_edges.append((m.group(1), m.group(2)))

    def _keep(name: str) -> bool:
        return not any(name.startswith(p) for p in _SKIP_PREFIXES)

    graph: dict[str, set[str]] = {}
    for nid, name in node_name.items():
        if _keep(name):
            graph.setdefault(name, set())

    for src_id, dst_id in raw_edges:
        src = node_name.get(src_id)
        dst = node_name.get(dst_id)
        if src and dst and _keep(src):
            graph.setdefault(src, set())
            if _keep(dst):
                graph[src].add(dst)

    return graph


# ---------------------------------------------------------------------------
# Output formatters
# ---------------------------------------------------------------------------

def cmd_list(graph: dict[str, set[str]]) -> int:
    for func in sorted(graph):
        callees = sorted(graph[func])
        if callees:
            print(func)
            for c in callees:
                print(f"  → {c}")
        else:
            print(f"{func}  [leaf]")
    return 0


def cmd_from(graph: dict[str, set[str]], root: str, depth: int | None) -> int:
    if root not in graph:
        matches = [f for f in graph if root in f]
        if not matches:
            _die(f"Function '{root}' not found in call graph.\n"
                 f"  Hint: run 'list' to see all function names.")
        if len(matches) == 1:
            print(f"Note: using '{matches[0]}' (substring match)", file=sys.stderr)
            root = matches[0]
        else:
            print(f"ERROR: '{root}' is ambiguous — matches:", file=sys.stderr)
            for m in sorted(matches):
                print(f"  {m}", file=sys.stderr)
            sys.exit(1)

    seen: set[str] = set()
    limit = depth if depth is not None else float("inf")

    def _tree(func: str, indent: int, remaining) -> None:
        pad = "  " * indent
        if func in seen:
            print(f"{pad}{func}  [↑ see above]")
            return
        seen.add(func)
        print(f"{pad}{func}")
        if remaining == 0:
            if graph.get(func):
                print(f"{pad}  … (depth limit reached)")
            return
        for callee in sorted(graph.get(func, [])):
            _tree(callee, indent + 1, remaining - 1)

    _tree(root, 0, limit)
    return 0


def cmd_dot(graph: dict[str, set[str]]) -> int:
    print('digraph callgraph {')
    for func, callees in sorted(graph.items()):
        for callee in sorted(callees):
            f = func.replace('"', '\\"')
            c = callee.replace('"', '\\"')
            print(f'  "{f}" -> "{c}";')
    print('}')
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def _die(msg: str) -> None:
    print(f"ERROR: {msg}", file=sys.stderr)
    sys.exit(1)


USAGE = """\
Usage:
  callgraph.py list  <compile_commands.json>
  callgraph.py from  <compile_commands.json> <function> [--depth N]
  callgraph.py dot   <compile_commands.json>
  callgraph.py --help
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

    if cmd == "list":
        if len(rest) != 1:
            _die("list requires exactly one argument: <compile_commands.json>")
        graph = _build_graph(rest[0])
        sys.exit(cmd_list(graph))

    elif cmd == "from":
        if len(rest) < 2:
            _die("from requires: <compile_commands.json> <function> [--depth N]")
        cc, func = rest[0], rest[1]
        depth = None
        i = 2
        while i < len(rest):
            if rest[i] == "--depth" and i + 1 < len(rest):
                try:
                    depth = int(rest[i + 1])
                except ValueError:
                    _die(f"--depth must be an integer, got '{rest[i+1]}'")
                i += 2
            else:
                _die(f"unknown argument '{rest[i]}'")
        graph = _build_graph(cc)
        sys.exit(cmd_from(graph, func, depth))

    elif cmd == "dot":
        if len(rest) != 1:
            _die("dot requires exactly one argument: <compile_commands.json>")
        graph = _build_graph(rest[0])
        sys.exit(cmd_dot(graph))

    else:
        _die(f"unknown subcommand '{cmd}'. Run with --help for usage.")


if __name__ == "__main__":
    main()
