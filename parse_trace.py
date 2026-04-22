#!/usr/bin/env python3
"""
parse_trace.py — Parser for Claude Code agentic session trace files.

Trace files are mixed-format: plain text log lines interleaved with
JSON Lines, one JSON object per line. This parser extracts the JSON
events and reconstructs the structured agent execution.

Usage:
    python3 parse_trace.py trace_c.txt
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Literal, Optional


# ---------------------------------------------------------------------------
# Data Structures
# ---------------------------------------------------------------------------

@dataclass
class TokenUsage:
    input_tokens: int
    output_tokens: int
    cache_creation_input_tokens: int
    cache_read_input_tokens: int


@dataclass
class ProgressSnapshot:
    """
    A framework-emitted progress update for a running sub-agent.
    These are emitted as system/task_progress events and do NOT enter
    the Claude API conversation — they are observability artifacts only.
    """
    total_tokens: int
    tool_uses: int
    duration_ms: int
    last_tool_name: str
    description: str


@dataclass
class ToolResult:
    tool_use_id: str   # matches the ToolUse.id that produced this result
    content: str
    is_error: bool = False


@dataclass
class SubAgent:
    """
    A sub-agent invocation embedded inside an Agent tool call.

    BLOCKING SEMANTICS: A SubAgent is NOT truly asynchronous. While it runs,
    the parent agent's API call loop is completely paused — no new turns are
    issued to the parent until this SubAgent finishes and returns its result.
    From the parent's perspective, Agent is just a slow tool: it blocks until
    done, then delivers a tool_result exactly like Bash or Read would.

    The sub-agent's full execution is a recursive list[Turn], embedded here
    rather than at the Session level. This tree structure naturally handles
    deeper nesting (sub-agents spawning their own sub-agents) without any
    change to the schema.

    Linkage: SubAgent.tool_use_id == the parent ToolUse.id that spawned it.
    This is the same ID that appears in system/task_started,
    system/task_progress, system/task_notification, and the final user
    tool_result that returns control to the parent.
    """
    task_id: str
    tool_use_id: str
    description: str
    prompt: str
    status: str

    # Full nested conversation of the sub-agent (same structure as parent)
    conversation: list[Turn] = field(default_factory=list)

    # Framework-level progress snapshots (not API messages)
    progress_snapshots: list[ProgressSnapshot] = field(default_factory=list)

    # Aggregate stats from system/task_notification
    total_tokens: int = 0
    total_tool_uses: int = 0
    duration_ms: int = 0


@dataclass
class ToolUse:
    """
    One tool invocation within a Turn.

    PARALLEL EXECUTION: Multiple ToolUse objects may exist within a single
    Turn. When the model outputs multiple tool_use blocks in one API
    response, Claude Code executes them concurrently on the local machine.
    However, the Turn boundary is strictly synchronous: ALL ToolUse objects
    in a Turn must complete before the next Turn's API request is sent.

    Matching: ToolUse.id (a.k.a. tool_use_id) is the join key between a
    tool_use content block and its tool_result. Matching is by ID, not by
    position, so parallel results that arrive out of order are handled
    correctly.

    Sub-agent case: when name == "Agent", this ToolUse spawns a sub-agent.
    The subtask field holds the full nested execution. The parent turn does
    not advance until subtask completes and result is populated.
    """
    id: str       # tool_use_id
    name: str     # e.g. "Bash", "Read", "Write", "Agent", "Edit"
    input: dict

    # Populated after local execution (or sub-agent completion)
    result: Optional[ToolResult] = None

    # Populated only when name == "Agent"
    subagent: Optional[SubAgent] = None


@dataclass
class ContentBlock:
    """
    One content block within an API response, in server emission order.

    Within a single Turn, the server streams blocks in a fixed order:
        thinking* → text* → tool_use*

    Claude API enforces that all thinking blocks precede any tool_use block.
    Preserving this ordering allows downstream analysis of the model's
    reasoning-before-action pattern.

    Each ContentBlock corresponds to one JSON record in the trace file
    (Claude Code emits each block as a separate streaming event), but all
    blocks in a Turn share the same logical API response.
    """
    type: Literal["thinking", "text", "tool_use"]
    thinking: Optional[str] = None
    text: Optional[str] = None
    tool_use: Optional[ToolUse] = None


@dataclass
class Turn:
    """
    One complete API request-response cycle for a given agent.

    Exactly one HTTP request is sent to the Claude API per Turn; the
    response may be streamed as multiple JSON events (one per ContentBlock)
    but they are all part of the same Turn.

    SYNCHRONOUS BARRIER: A new Turn does not begin until every ToolUse
    from the previous Turn has completed and returned a result. This
    includes Agent tool calls (sub-agents), which may internally span
    many turns of their own. The next API request carries the full
    accumulated context: all prior content + all tool results.

    turn_index counts only the turns of the agent that owns this
    conversation. Sub-agent turns (nested inside a SubAgent) are counted
    separately in their own conversation and never contribute to the
    parent's index. This matches the num_turns field in the result event.
    """
    turn_index: int
    content_blocks: list[ContentBlock] = field(default_factory=list)

    # Usage from the last streaming event of this turn's assistant records
    # (Claude Code echoes usage on each streamed block; only the final
    # value is meaningful as the cumulative total for the API call)
    usage: Optional[TokenUsage] = None

    @property
    def tool_uses(self) -> list[ToolUse]:
        return [b.tool_use for b in self.content_blocks
                if b.type == "tool_use" and b.tool_use is not None]

    @property
    def thinking_texts(self) -> list[str]:
        return [b.thinking for b in self.content_blocks
                if b.type == "thinking" and b.thinking is not None]

    @property
    def texts(self) -> list[str]:
        return [b.text for b in self.content_blocks
                if b.type == "text" and b.text is not None]

    @property
    def is_final(self) -> bool:
        """True if this turn produced no tool calls (conversation end)."""
        return len(self.tool_uses) == 0


@dataclass
class MonitoringEvent:
    """
    A framework-level event that does NOT enter the Claude API conversation.

    system/init, system/task_started, system/task_progress,
    system/task_notification, and rate_limit_event records are emitted by
    the Claude Code framework for observability. They share the session_id
    but are never included in API requests. Keeping them in a separate list
    makes it easy to reconstruct the pure API conversation history.
    """
    type: str
    subtype: Optional[str]
    line_number: int
    raw: dict


@dataclass
class InitEvent:
    model: str
    cwd: str
    permission_mode: str
    claude_code_version: str
    tool_names: list[str]


@dataclass
class ModelUsage:
    model: str
    input_tokens: int
    output_tokens: int
    cache_read_tokens: int
    cache_creation_tokens: int
    cost_usd: float


@dataclass
class ResultEvent:
    is_error: bool
    stop_reason: str
    num_turns: int
    duration_ms: int
    duration_api_ms: int
    total_cost_usd: float
    result_text: str
    model_usage: dict[str, ModelUsage]  # keyed by model name


@dataclass
class Session:
    """
    One complete Claude Code agent process lifecycle.

    A Session spans from system/init to the result event. Every JSON record
    sharing the same session_id belongs to one Session.

    SEPARATION OF CONCERNS:
    - conversation: only the top-level agent's Turns (pure API history)
    - monitoring:   framework events that never entered the API
    Sub-agent turns live recursively inside ToolUse.subtask.conversation,
    not in this list. This preserves the tree structure of nested agents
    while keeping the top-level view clean.
    """
    session_id: str
    phase: str   # "translation" | "verification" | "unknown"

    init: Optional[InitEvent] = None
    conversation: list[Turn] = field(default_factory=list)
    monitoring: list[MonitoringEvent] = field(default_factory=list)
    result: Optional[ResultEvent] = None


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------

def _extract_tool_result_content(raw_content) -> str:
    if isinstance(raw_content, str):
        return raw_content
    if isinstance(raw_content, list):
        return "\n".join(
            item.get("text", "") if isinstance(item, dict) else str(item)
            for item in raw_content
        )
    return str(raw_content)


def _parse_token_usage(raw: dict) -> TokenUsage:
    return TokenUsage(
        input_tokens=raw.get("input_tokens", 0),
        output_tokens=raw.get("output_tokens", 0),
        cache_creation_input_tokens=raw.get("cache_creation_input_tokens", 0),
        cache_read_input_tokens=raw.get("cache_read_input_tokens", 0),
    )


class TraceParser:
    def parse_file(self, path: str) -> list[Session]:
        """Read the trace file and return one Session per session_id."""
        records: list[tuple[int, dict]] = []
        with open(path, "r") as f:
            for lineno, line in enumerate(f, 1):
                stripped = line.strip()
                if stripped.startswith("{"):
                    try:
                        records.append((lineno, json.loads(stripped)))
                    except json.JSONDecodeError:
                        pass

        by_session: dict[str, list[tuple[int, dict]]] = defaultdict(list)
        for lineno, obj in records:
            sid = obj.get("session_id", "unknown")
            by_session[sid].append((lineno, obj))

        sessions = []
        for idx, (sid, events) in enumerate(by_session.items()):
            phases = ["translation", "verification", "unknown"]
            phase = phases[min(idx, 2)]
            sessions.append(self._parse_session(sid, phase, events))
        return sessions

    def _parse_session(
        self, session_id: str, phase: str, events: list[tuple[int, dict]]
    ) -> Session:
        session = Session(session_id=session_id, phase=phase)

        # ------------------------------------------------------------------
        # Step 1: Identify sub-agent brackets.
        #
        # A sub-agent bracket is the range of event indices [start+1, end)
        # between a system/task_started and its matching task_notification,
        # both carrying the same tool_use_id.
        # Records inside a bracket belong to the sub-agent's conversation.
        # ------------------------------------------------------------------
        subagent_map: dict[str, SubAgent] = {}   # tool_use_id -> SubAgent
        task_stack: dict[str, int] = {}        # tool_use_id -> event index
        task_brackets: dict[str, tuple[int, int]] = {}  # tool_use_id -> (start, end)

        for i, (lineno, obj) in enumerate(events):
            sub = obj.get("subtype")
            if sub == "task_started":
                tid = obj.get("tool_use_id", "")
                if tid:
                    task_stack[tid] = i
                    subagent_map[tid] = SubAgent(
                        task_id=obj.get("task_id", ""),
                        tool_use_id=tid,
                        description=obj.get("description", ""),
                        prompt=obj.get("prompt", ""),
                        status="running",
                    )
            elif sub == "task_notification":
                tid = obj.get("tool_use_id", "")
                if tid and tid in task_stack:
                    start_idx = task_stack.pop(tid)
                    task_brackets[tid] = (start_idx, i)
                    st = subagent_map[tid]
                    st.status = obj.get("status", "completed")
                    usage = obj.get("usage", {})
                    st.total_tokens = usage.get("total_tokens", 0)
                    st.total_tool_uses = usage.get("tool_uses", 0)
                    st.duration_ms = usage.get("duration_ms", 0)

        # Map each event index to the sub-agent tool_use_id it belongs to
        subagent_index: dict[int, str] = {}
        for tid, (start, end) in task_brackets.items():
            for i in range(start + 1, end):
                subagent_index[i] = tid

        # ------------------------------------------------------------------
        # Step 2: Route each event to main conversation, sub-agent bucket,
        # or monitoring.
        # ------------------------------------------------------------------
        main_records: list[tuple[int, dict]] = []
        subagent_records: dict[str, list[tuple[int, dict]]] = defaultdict(list)

        for i, (lineno, obj) in enumerate(events):
            typ = obj.get("type")
            sub = obj.get("subtype")

            if typ in ("system", "rate_limit_event"):
                session.monitoring.append(MonitoringEvent(
                    type=typ, subtype=sub, line_number=lineno, raw=obj,
                ))
                if sub == "init":
                    session.init = self._parse_init(obj)
                elif sub == "task_progress":
                    tid = obj.get("tool_use_id", "")
                    if tid in subagent_map:
                        usage = obj.get("usage", {})
                        subagent_map[tid].progress_snapshots.append(ProgressSnapshot(
                            total_tokens=usage.get("total_tokens", 0),
                            tool_uses=usage.get("tool_uses", 0),
                            duration_ms=usage.get("duration_ms", 0),
                            last_tool_name=obj.get("last_tool_name", "") or "",
                            description=obj.get("description", "") or "",
                        ))
                continue

            if typ == "result":
                session.result = self._parse_result(obj)
                continue

            if typ not in ("assistant", "user"):
                continue

            if i in subagent_index:
                subagent_records[subagent_index[i]].append((lineno, obj))
            else:
                main_records.append((lineno, obj))

        # ------------------------------------------------------------------
        # Step 3: Build conversations.
        # ------------------------------------------------------------------
        session.conversation = self._parse_conversation(main_records)

        for tid, records in subagent_records.items():
            if tid in subagent_map:
                subagent_map[tid].conversation = self._parse_conversation(records)

        # Attach SubAgent objects to their parent Agent ToolUse
        self._attach_subagents(session.conversation, subagent_map)

        return session

    def _parse_conversation(self, records: list[tuple[int, dict]]) -> list[Turn]:
        """
        Build a Turn list from a flat sequence of assistant/user records.

        A new Turn begins at each assistant record that follows a non-
        assistant record. Consecutive assistant records belong to the same
        Turn (they are streaming chunks of one API response).

        Tool results are matched to ToolUse by tool_use_id, not by position,
        so parallel tool calls with out-of-order results are handled correctly.
        """
        turns: list[Turn] = []
        i = 0

        while i < len(records):
            _, obj = records[i]
            if obj.get("type") != "assistant":
                i += 1
                continue

            turn = Turn(turn_index=len(turns) + 1)
            pending: dict[str, ToolUse] = {}   # tool_use_id -> ToolUse
            last_usage: Optional[TokenUsage] = None

            # Collect consecutive assistant records (one API response)
            while i < len(records) and records[i][1].get("type") == "assistant":
                msg = records[i][1].get("message", {})
                raw_usage = msg.get("usage")
                if raw_usage:
                    last_usage = _parse_token_usage(raw_usage)
                for block in msg.get("content", []):
                    btype = block.get("type")
                    if btype == "thinking":
                        turn.content_blocks.append(ContentBlock(
                            type="thinking", thinking=block.get("thinking", ""),
                        ))
                    elif btype == "text":
                        turn.content_blocks.append(ContentBlock(
                            type="text", text=block.get("text", ""),
                        ))
                    elif btype == "tool_use":
                        tu = ToolUse(
                            id=block.get("id", ""),
                            name=block.get("name", ""),
                            input=block.get("input", {}),
                        )
                        pending[tu.id] = tu
                        turn.content_blocks.append(ContentBlock(
                            type="tool_use", tool_use=tu,
                        ))
                i += 1

            turn.usage = last_usage

            # Collect following user records (tool results for this turn)
            while i < len(records) and records[i][1].get("type") == "user":
                msg = records[i][1].get("message", {})
                for block in msg.get("content", []):
                    if block.get("type") == "tool_result":
                        tid = block.get("tool_use_id", "")
                        result = ToolResult(
                            tool_use_id=tid,
                            content=_extract_tool_result_content(block.get("content", "")),
                            is_error=block.get("is_error", False),
                        )
                        if tid in pending:
                            pending[tid].result = result
                i += 1

            turns.append(turn)

        return turns

    def _attach_subagents(
        self, turns: list[Turn], subagent_map: dict[str, SubAgent]
    ) -> None:
        """Wire SubAgent objects into their parent Agent ToolUse nodes."""
        for turn in turns:
            for block in turn.content_blocks:
                if block.type == "tool_use" and block.tool_use is not None:
                    tu = block.tool_use
                    if tu.name == "Agent" and tu.id in subagent_map:
                        tu.subagent = subagent_map[tu.id]

    def _parse_init(self, obj: dict) -> InitEvent:
        tools = obj.get("tools", [])
        tool_names = [
            t.get("name", "") if isinstance(t, dict) else str(t) for t in tools
        ]
        return InitEvent(
            model=obj.get("model", ""),
            cwd=obj.get("cwd", ""),
            permission_mode=obj.get("permissionMode", ""),
            claude_code_version=obj.get("claude_code_version", ""),
            tool_names=tool_names,
        )

    def _parse_result(self, obj: dict) -> ResultEvent:
        model_usage = {}
        for name, mu in obj.get("modelUsage", {}).items():
            model_usage[name] = ModelUsage(
                model=name,
                input_tokens=mu.get("inputTokens", 0),
                output_tokens=mu.get("outputTokens", 0),
                cache_read_tokens=mu.get("cacheReadInputTokens", 0),
                cache_creation_tokens=mu.get("cacheCreationInputTokens", 0),
                cost_usd=mu.get("costUSD", 0.0),
            )
        return ResultEvent(
            is_error=obj.get("is_error", False),
            stop_reason=obj.get("stop_reason", ""),
            num_turns=obj.get("num_turns", 0),
            duration_ms=obj.get("duration_ms", 0),
            duration_api_ms=obj.get("duration_api_ms", 0),
            total_cost_usd=obj.get("total_cost_usd", 0.0),
            result_text=obj.get("result", ""),
            model_usage=model_usage,
        )


# ---------------------------------------------------------------------------
# Validation / Statistics
# ---------------------------------------------------------------------------

def count_tools_in_conversation(turns: list[Turn]) -> dict[str, int]:
    counts: dict[str, int] = defaultdict(int)
    for turn in turns:
        for tu in turn.tool_uses:
            counts[tu.name] += 1
    return dict(counts)


def print_session_stats(sessions: list[Session]) -> None:
    print(f"Sessions parsed: {len(sessions)}")
    print()

    for s in sessions:
        r = s.result
        print(f"{'=' * 60}")
        print(f"Session [{s.phase}]  id={s.session_id[:8]}...")
        print(f"{'=' * 60}")

        if s.init:
            print(f"  Model:          {s.init.model}")
            print(f"  CC version:     {s.init.claude_code_version}")
            print(f"  Tools available:{len(s.init.tool_names)}")

        main_turns = len(s.conversation)
        main_with_thinking = sum(1 for t in s.conversation if t.thinking_texts)
        main_tool_uses = count_tools_in_conversation(s.conversation)
        agent_calls = [
            tu for t in s.conversation for tu in t.tool_uses if tu.name == "Agent"
        ]

        print(f"\n  --- Main conversation ---")
        print(f"  Turns (main agent API calls): {main_turns}")
        print(f"  Turns with extended thinking: {main_with_thinking}")
        print(f"  Tool calls by name:           {dict(sorted(main_tool_uses.items()))}")
        print(f"  Agent (sub-agent) calls:      {len(agent_calls)}")

        for ag in agent_calls:
            st = ag.subagent
            if st:
                sub_turns = len(st.conversation)
                sub_tools = count_tools_in_conversation(st.conversation)
                prog_steps = len(st.progress_snapshots)
                print(f"\n  --- Sub-agent [{st.description}] ---")
                print(f"    task_id:        {st.task_id}")
                print(f"    status:         {st.status}")
                print(f"    turns:          {sub_turns}")
                print(f"    tool calls:     {dict(sorted(sub_tools.items()))}")
                print(f"    progress snaps: {prog_steps}")
                print(f"    total_tokens:   {st.total_tokens}")
                print(f"    total_tool_uses:{st.total_tool_uses}")
                print(f"    duration_ms:    {st.duration_ms}")

        monitoring_subtypes: dict[str, int] = defaultdict(int)
        for m in s.monitoring:
            key = f"{m.type}/{m.subtype}" if m.subtype else m.type
            monitoring_subtypes[key] += 1
        print(f"\n  --- Monitoring events ---")
        for k, v in sorted(monitoring_subtypes.items()):
            print(f"    {k}: {v}")

        if r:
            print(f"\n  --- Result ---")
            print(f"    stop_reason:    {r.stop_reason}")
            print(f"    is_error:       {r.is_error}")
            print(f"    num_turns:      {r.num_turns}")
            print(f"    duration_ms:    {r.duration_ms}")
            print(f"    total_cost_usd: ${r.total_cost_usd:.4f}")
            for model, mu in r.model_usage.items():
                print(f"    [{model}]")
                print(f"      output_tokens: {mu.output_tokens}")
                print(f"      cache_read:    {mu.cache_read_tokens}")
                print(f"      cost_usd:      ${mu.cost_usd:.4f}")
        print()


# ---------------------------------------------------------------------------
# Human-readable history printer
# ---------------------------------------------------------------------------

def _truncate(text: str, max_len: int = 30000) -> str:
    text = text.strip()
    if len(text) <= max_len:
        return text
    return text[:max_len] + f"  …[+{len(text) - max_len} chars]"


def _fmt_tool_input(name: str, inp: dict) -> str:
    """Format tool input as a compact one-or-few-liner."""
    if name == "Bash":
        cmd = inp.get("command", "").strip().replace("\n", " ↵ ")
        desc = inp.get("description", "")
        if desc:
            return f"[{desc}]\n         $ {_truncate(cmd)}"
        return f"$ {_truncate(cmd)}"
    if name in ("Read", "Write", "Edit"):
        path = inp.get("file_path", inp.get("path", ""))
        extra = ""
        if name == "Write":
            content = inp.get("content", "")
            extra = f"  ({len(content)} chars)"
        elif name == "Edit":
            old = inp.get("old_string", "")[:60].replace("\n", "↵")
            new = inp.get("new_string", "")[:60].replace("\n", "↵")
            extra = f"\n         - {old!r}\n         + {new!r}"
        return f"{path}{extra}"
    if name == "Agent":
        desc = inp.get("description", "")
        sub_type = inp.get("subagent_type", "")
        prompt_preview = _truncate(inp.get("prompt", ""))
        return f"[{sub_type}] {desc}\n         prompt: {prompt_preview}"
    # Generic fallback
    parts = []
    for k, v in inp.items():
        v_str = str(v).replace("\n", "↵")
        parts.append(f"{k}={_truncate(v_str)}")
    return "  ".join(parts)


def _fmt_turns(turns: list[Turn], out: list[str], indent: str = "") -> None:
    """Append formatted turn lines into `out` (a list[str] for efficient joining)."""
    for turn in turns:
        out.append(f"{indent}┌─ Turn {turn.turn_index} {'─' * max(0, 60 - len(indent) - 10)}")

        # Thinking blocks
        for thinking in turn.thinking_texts:
            preview = _truncate(thinking)
            out.append(f"{indent}│  💭 Thinking: ({len(thinking)} chars)")
            for line in preview.splitlines():
                out.append(f"{indent}│     {line}")

        # Text blocks
        for text in turn.texts:
            out.append(f"{indent}│  💬 {_truncate(text)}")

        # Tool calls
        for tu in turn.tool_uses:
            is_agent = tu.name == "Agent"
            icon = "🤖" if is_agent else "🔧"
            fmt_input = _fmt_tool_input(tu.name, tu.input)
            input_lines = fmt_input.splitlines()
            out.append(f"{indent}│  {icon} Tool: {tu.name}  {input_lines[0]}")
            for extra_line in input_lines[1:]:
                out.append(f"{indent}│     {extra_line}")

            # Sub-agent: recurse with deeper indentation
            if is_agent and tu.subagent:
                sa = tu.subagent
                out.append(f"{indent}│  ╔═ Sub-agent [{sa.description}] ({'─'*20})")
                out.append(f"{indent}│  ║  task_id: {sa.task_id}  status: {sa.status}")
                _fmt_turns(sa.conversation, out, indent=indent + "│  ║  ")
                out.append(
                    f"{indent}│  ╚═ Sub-agent done  "
                    f"tokens={sa.total_tokens}  tools={sa.total_tool_uses}  "
                    f"duration={sa.duration_ms}ms"
                )

            # Tool result (after sub-agent block when applicable)
            if tu.result:
                result_text = _truncate(tu.result.content)
                err_tag = " [ERROR]" if tu.result.is_error else ""
                out.append(f"{indent}│  📥 Result{err_tag}: ({len(tu.result.content)} chars)")
                for line in result_text.splitlines():
                    out.append(f"{indent}│     {line}")

        out.append(f"{indent}└{'─' * max(0, 62 - len(indent))}")
        out.append("")


def build_readable_history(sessions: list[Session]) -> str:
    """
    Build a human-readable narrative of the full agent execution.
    Returns a single string. Uses list accumulation + join for efficiency
    since the output can be very large (multiple MB).
    """
    out: list[str] = []
    total = len(sessions)

    for idx, s in enumerate(sessions, 1):
        r = s.result
        duration_s = f"{r.duration_ms / 1000:.1f}s" if r else "?"
        cost = f"${r.total_cost_usd:.4f}" if r else "?"

        out.append("")
        out.append("=" * 70)
        out.append(f"  Session {idx}/{total} — {s.phase.upper()}")
        out.append(
            f"  model: {s.init.model if s.init else '?'}  "
            f"duration: {duration_s}  cost: {cost}"
        )
        out.append("=" * 70)
        out.append("")

        # Surface rate-limit events
        for m in s.monitoring:
            if m.type == "rate_limit_event" or m.subtype == "rate_limit_event":
                info = m.raw.get("rate_limit_info", {})
                out.append(
                    f"  ⚠️  Rate limit: status={info.get('status')}  "
                    f"type={info.get('rateLimitType')}"
                )

        out.append("")
        _fmt_turns(s.conversation, out)

        if r:
            out.append(
                f"  ✅ Session ended: {r.stop_reason}  "
                f"turns={r.num_turns}  cost={cost}"
            )
            for model, mu in r.model_usage.items():
                short = model.split("-")[1] if "-" in model else model
                out.append(
                    f"     [{short}] out={mu.output_tokens}  "
                    f"cache_read={mu.cache_read_tokens}  ${mu.cost_usd:.4f}"
                )
        out.append("")

    return "\n".join(out)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    args = sys.argv[1:]
    if not args:
        print("Usage: parse_trace.py <file> [--readable]")
        sys.exit(1)

    path = args[0]
    readable = "--readable" in args or "-r" in args

    parser = TraceParser()
    sessions = parser.parse_file(path)

    if readable:
        out_path = path.rsplit(".", 1)[0] + "_readable.txt"
        content = build_readable_history(sessions)
        with open(out_path, "w") as f:
            f.write(content)
        print(f"Written to {out_path}  ({len(content):,} chars)")
    else:
        print_session_stats(sessions)
