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

import html
import json
import re
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

    SYNC vs ASYNC:
    - Synchronous (default; `run_in_background` not set or false): the parent's
      API call loop blocks until this SubAgent finishes and returns its result.
      Its full conversation is recorded in the trace as assistant/user records
      tagged with `parent_tool_use_id` — so `conversation` is fully populated.
    - Asynchronous (`run_in_background: true`): the parent receives an
      "Async agent launched" tool_result immediately and continues working.
      The async agent's actual conversation is NOT in the trace; only
      `task_progress` snapshots are emitted (one per tool call inside it)
      describing what it was doing. For these, `conversation` is synthesized
      from `progress_snapshots` so the visualizer has something to show.

    The sub-agent's execution is a recursive list[Turn], embedded here rather
    than at the Session level. This tree naturally handles deeper nesting
    (sub-agents spawning their own sub-agents) without schema changes.

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

    # Full nested conversation of the sub-agent (same structure as parent).
    # For async sub-agents this is synthesized from progress_snapshots and
    # is_async is set True.
    conversation: list[Turn] = field(default_factory=list)
    is_async: bool = False

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

    # When set, overrides the size computed from `result.content` etc.
    # Used for synthesized ToolUses (async sub-agent progress events) where
    # the real result content isn't in the trace; we substitute a token-delta
    # estimate so the bar segment has a meaningful width.
    size_override: Optional[int] = None


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
class CompactEvent:
    """
    A context-window compaction (`system/compact_boundary` event).

    Claude Code summarizes the prior conversation when context approaches the
    model's limit. The next API request then carries this compressed summary
    instead of the full history — ~95% smaller in our traces. Mark these as
    discontinuities: facts the agent established before a compact may be
    represented only fuzzily afterwards.
    """
    pre_tokens: int       # context size before compaction (tokens)
    post_tokens: int      # context size after compaction (tokens)
    duration_ms: int      # time the compaction itself took
    trigger: str          # "auto" or "manual"
    line_number: int

    # 1-based turn index AFTER which this compaction occurred. 0 means
    # "before any main-conversation turns". Populated post-parse by counting
    # main-conversation turn boundaries with line numbers below this event.
    after_turn_index: int = 0


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
    compact_events: list[CompactEvent] = field(default_factory=list)
    result: Optional[ResultEvent] = None

    # Wall-clock duration in ms, derived from the difference between the
    # earliest and latest event `timestamp` for this session_id. This is the
    # only field that matches "real time elapsed". The framework's
    # `result.duration_ms` is broken for async-heavy sessions (only counts
    # main-agent-active time), and `result.duration_api_ms` double-counts
    # parallel sub-agent work.
    wall_clock_ms: int = 0


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------

def _wall_clock_ms(events: list[tuple[int, dict]]) -> int:
    """Span between the earliest and latest event `timestamp` (ISO 8601 with
    Z suffix) across `events`. Returns 0 if no timestamps are available.

    Used as the only honest "duration of this session" measurement, because
    framework-reported duration_ms breaks for async sub-agents and
    duration_api_ms double-counts parallel sub-agent work."""
    from datetime import datetime
    first_ts = None
    last_ts = None
    for _, obj in events:
        ts_str = obj.get("timestamp")
        if not ts_str:
            continue
        try:
            t = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        except (TypeError, ValueError):
            continue
        if first_ts is None or t < first_ts:
            first_ts = t
        if last_ts is None or t > last_ts:
            last_ts = t
    if first_ts is None or last_ts is None:
        return 0
    return int((last_ts - first_ts).total_seconds() * 1000)


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
                elif sub == "compact_boundary" and i not in subagent_index:
                    md = obj.get("compact_metadata") or {}
                    session.compact_events.append(CompactEvent(
                        pre_tokens=md.get("pre_tokens", 0),
                        post_tokens=md.get("post_tokens", 0),
                        duration_ms=md.get("duration_ms", 0),
                        trigger=md.get("trigger", "") or "",
                        line_number=lineno,
                    ))
                continue

            if typ == "result":
                session.result = self._parse_result(obj)
                continue

            if typ not in ("assistant", "user"):
                continue

            # Route by parent_tool_use_id, NOT by task_brackets index ranges.
            # When the main agent dispatches a second sub-agent before the first
            # one finishes (e.g. it doesn't strictly wait), the brackets overlap
            # and `subagent_index[i] = tid` becomes last-write-wins, mis-routing
            # any record (including the parent's own tool_result) that falls in
            # the overlap. parent_tool_use_id is the authoritative pointer:
            # main-agent records have it empty, sub-agent records carry their
            # parent's tool_use_id. See c17 T17/T19 for the failure mode.
            parent_tid = obj.get("parent_tool_use_id") or ""
            if parent_tid:
                subagent_records[parent_tid].append((lineno, obj))
                # Be safe: a parent_tool_use_id we haven't seen via task_started
                # would otherwise have no SubAgent metadata. Create a stub.
                if parent_tid not in subagent_map:
                    subagent_map[parent_tid] = SubAgent(
                        task_id="", tool_use_id=parent_tid,
                        description="", prompt="", status="unknown",
                    )
            else:
                main_records.append((lineno, obj))

        # ------------------------------------------------------------------
        # Step 3: Build conversations.
        # ------------------------------------------------------------------
        session.conversation = self._parse_conversation(main_records)

        # Locate each compact event in the main turn sequence by counting how
        # many turn boundaries fall before its line number. A turn boundary
        # is an assistant record that does not immediately follow another
        # assistant record (consecutive assistant records are streamed chunks
        # of one API response).
        turn_start_linenos: list[int] = []
        prev_was_assistant = False
        for lineno, obj in main_records:
            is_assistant = obj.get("type") == "assistant"
            if is_assistant and not prev_was_assistant:
                turn_start_linenos.append(lineno)
            prev_was_assistant = is_assistant
        for ev in session.compact_events:
            ev.after_turn_index = sum(
                1 for ts in turn_start_linenos if ts < ev.line_number
            )

        for tid, records in subagent_records.items():
            if tid in subagent_map:
                subagent_map[tid].conversation = self._parse_conversation(records)

        # Async sub-agents (`run_in_background: true`) leave no parented
        # assistant/user records — the trace only carries `task_progress`
        # snapshots. For each such sub-agent, synthesize an approximate turn
        # list from its progress snapshots so the visualizer can show the
        # internal activity (one row per progress snapshot).
        for tid, sa in subagent_map.items():
            if not sa.conversation and sa.progress_snapshots:
                sa.is_async = True
                sa.conversation = _synthesize_async_subagent_turns(
                    sa.progress_snapshots
                )

        # Attach SubAgent objects to their parent Agent ToolUse
        self._attach_subagents(session.conversation, subagent_map)

        # Compute true wall-clock duration: span between earliest and latest
        # `timestamp` field across all events for this session_id. Robust
        # against async sub-agents (frame's duration_ms misses idle time)
        # and against parallelism (frame's duration_api_ms double-counts).
        session.wall_clock_ms = _wall_clock_ms(events)

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
                print(f"      input_tokens:  {mu.input_tokens}")
                print(f"      output_tokens: {mu.output_tokens}")
                print(f"      cache_read:    {mu.cache_read_tokens}")
                print(f"      cache_create:  {mu.cache_creation_tokens}")
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



# ---------------------------------------------------------------------------
# File I/O Analysis (--file-io)
# ---------------------------------------------------------------------------

def _normalize_io_path(path: str) -> str:
    """Normalize /tmp/.tmpXXX/translated_rust/... to relative form."""
    path = re.sub(r'^/tmp/\.tmp\w+/translated_rust/', '', path)
    path = re.sub(r'^\./', '', path)
    return path.strip()


def _extract_bash_files(cmd: str) -> list[str]:
    """Extract file paths referenced in a Bash command."""
    files = set()
    for m in re.finditer(r'(?:c_src|src)/[\w./-]+\.(?:c|h|rs|toml)', cmd):
        files.add(m.group(0))
    for m in re.finditer(r'/tmp/\.tmp\w+/translated_rust/([\w./-]+\.(?:c|h|rs|toml))', cmd):
        files.add(m.group(1))
    return list(files)


def _estimate_read_bytes(offset: int, limit: int) -> int:
    return max(0, (limit - offset + 1)) * 80


def _flatten_tool_uses(sessions: list[Session]) -> list[tuple[str, ToolUse]]:
    """Yield (session_id, ToolUse) recursively including sub-agents."""
    stack: list[tuple[str, Session]] = [(s.session_id, s) for s in sessions]
    while stack:
        sid, sess = stack.pop()
        for turn in sess.conversation:
            for tu in turn.tool_uses:
                yield (sid, tu)
                if tu.subagent is not None:
                    stack.append((tu.subagent.task_id or sid, tu.subagent))


def generate_file_io_report(sessions: list[Session]) -> dict:
    """Return structured dict of per-file I/O patterns."""
    file_ops: dict[str, list[dict]] = defaultdict(list)

    for sid, tu in _flatten_tool_uses(sessions):
        inp = tu.input if isinstance(tu.input, dict) else {}
        ts = ''
        if tu.result and hasattr(tu.result, 'timestamp'):
            ts = tu.result.timestamp or ''

        if tu.name == 'Read':
            fp = inp.get('file_path', '')
            if not fp:
                continue
            np = _normalize_io_path(fp)
            offset = inp.get('offset', 1)
            limit = inp.get('limit', 500)
            if isinstance(offset, list): offset = offset[0] if offset else 1
            if isinstance(limit, list): limit = limit[0] if limit else 500
            if not isinstance(offset, int): offset = 1
            if not isinstance(limit, int): limit = 500
            file_ops[np].append({
                'op': 'read', 'session': sid, 'ts': ts,
                'bytes': _estimate_read_bytes(offset, limit),
                'offset': offset, 'limit': limit,
            })

        elif tu.name == 'Write':
            fp = inp.get('file_path', '')
            if not fp:
                continue
            np = _normalize_io_path(fp)
            clen = len(inp.get('content', '')) if isinstance(inp.get('content', ''), str) else 0
            file_ops[np].append({
                'op': 'write', 'session': sid, 'ts': ts, 'bytes': clen,
            })

        elif tu.name == 'Edit':
            fp = inp.get('file_path', '')
            if not fp:
                continue
            np = _normalize_io_path(fp)
            old_l = len(inp.get('old_string', '')) if isinstance(inp.get('old_string', ''), str) else 0
            new_l = len(inp.get('new_string', '')) if isinstance(inp.get('new_string', ''), str) else 0
            file_ops[np].append({
                'op': 'edit', 'session': sid, 'ts': ts,
                'old_len': old_l, 'new_len': new_l,
            })

        elif tu.name == 'Bash':
            cmd = inp.get('command', '')
            for fp in _extract_bash_files(cmd):
                np = _normalize_io_path(fp)
                file_ops[np].append({
                    'op': 'bash_ref', 'session': sid, 'ts': ts,
                    'cmd': cmd[:200],
                })

    files = {}
    for fp in sorted(file_ops.keys()):
        ops = file_ops[fp]
        reads = [o for o in ops if o['op'] == 'read']
        writes = [o for o in ops if o['op'] == 'write']
        edits = [o for o in ops if o['op'] == 'edit']
        bash_refs = [o for o in ops if o['op'] == 'bash_ref']
        files[fp] = {
            'read_count': len(reads),
            'write_count': len(writes),
            'edit_count': len(edits),
            'bash_ref_count': len(bash_refs),
            'total_read_bytes': sum(o.get('bytes', 0) for o in reads),
            'total_write_bytes': sum(o.get('bytes', 0) for o in writes),
            'sessions': sorted(set(o['session'][:8] for o in ops)),
            'ops': ops,
        }

    return {
        'files': files,
        'summary': {
            'total_files': len(files),
            'total_read_bytes': sum(f['total_read_bytes'] for f in files.values()),
            'total_write_bytes': sum(f['total_write_bytes'] for f in files.values()),
            'total_reads': sum(f['read_count'] for f in files.values()),
            'total_writes': sum(f['write_count'] for f in files.values()),
            'total_edits': sum(f['edit_count'] for f in files.values()),
            'total_bash_refs': sum(f['bash_ref_count'] for f in files.values()),
        },
    }


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
                    f"     [{short}] in={mu.input_tokens} out={mu.output_tokens}  "
                    f"cache_read={mu.cache_read_tokens}  ${mu.cost_usd:.4f}"
                )
        out.append("")

    return "\n".join(out)


# ---------------------------------------------------------------------------
# Timeline Visualization (SVG)
#
# Each turn becomes one horizontal bar. The bar is split into colored
# segments, one per operation, in execution order. Segment width is
# proportional to the character count that flowed through that operation:
#
#   read  — content read INTO context (tool_result of Read/Glob/Grep,
#           or Bash commands like cat/ls/grep that pull data in)
#   think — model output (thinking blocks + assistant text)
#   write — content the agent produced (Write content, Edit new_string,
#           Bash redirects/sed -i)
#   build — execution-style operations (cargo/cmake/make/gcc/python/...)
#           sized by tool_result length (the output that re-entered context)
#   other — anything we couldn't classify
#
# Bar TOTAL width is normalized to the longest turn in the trace, so a
# row that fills the whole strip = the heaviest turn; short rows = short
# turns. Hover any segment to see the full command/preview as a tooltip.
# ---------------------------------------------------------------------------

CAT_READ, CAT_THINK, CAT_WRITE, CAT_BUILD, CAT_SUBAGENT, CAT_OTHER = (
    "read", "think", "write", "build", "subagent", "other",
)

CAT_COLORS = {
    CAT_READ:     "#85c955",   # green
    CAT_THINK:    "#5b9bd5",   # blue
    CAT_WRITE:    "#e8a020",   # amber
    CAT_BUILD:    "#e05050",   # red
    CAT_SUBAGENT: "#9b80c8",   # purple-grey
    CAT_OTHER:    "#555555",   # dark grey
}

# Dark theme palette
_BG          = "#1e1e1e"
_ROW_ALT     = "#252525"
_TEXT        = "#cccccc"
_TEXT_DIM    = "#888888"
_GRID        = "#333333"
_TITLE       = "#e0e0e0"

# Bash command classification by leading word.
_READ_CMDS = {
    # Real shell commands
    "ls", "cat", "head", "tail", "find", "grep", "rg", "wc", "file",
    "stat", "du", "pwd", "which", "tree", "diff", "less", "more",
    "nm", "objdump", "readelf", "strings", "od", "xxd", "awk", "sed",
    "jq", "column", "sort", "uniq", "cut", "tr",
    "ps", "pgrep", "pidof", "ldd", "ldconfig",
    "echo", "printf",
    # English-verb intent (used by async sub-agent progress descriptions
    # that carry a prose summary instead of an actual shell command).
    "list", "show", "view", "check", "verify", "search", "count",
    "look", "examine", "read", "inspect", "scan", "lookup",
}
_WRITE_CMDS = {
    "cp", "mv", "mkdir", "touch", "rm", "rmdir", "ln", "chmod", "chown",
    "patch", "tar", "unzip", "zip",
    # English-verb intent
    "create", "delete", "remove", "write", "save", "make_dir", "rename",
}
_BUILD_CMDS = {
    "cargo", "cmake", "make", "ninja", "gcc", "clang", "g++", "clang++",
    "rustc", "rustfmt", "clippy", "go", "python", "python3", "node",
    "npm", "yarn", "pnpm",
    "ctest", "bear", "opt", "llvm-link", "llc", "ld", "ar",
    "bash", "sh", "zsh", "fish",
    # English-verb intent
    "compile", "build", "test", "run",
}


def _peel_bash(cmd: str) -> str:
    """Strip wrapper prefixes / loop scaffolding and return a simple head command.

    Handles: set -..., timeout, cd X (&&|newline), env, leading comments,
    backslash line-continuation, bare VAR=value assignments, echo "..." &&,
    and for/while/until ...; do BODY; done (returns BODY's head)."""
    cmd = cmd.strip()
    while True:
        new_cmd = cmd
        # Leading comment lines: "# foo\n..."
        new_cmd = re.sub(r"^#[^\n]*\n+", "", new_cmd)
        # Backslash line-continuation at start: "\\\n cmd"
        new_cmd = re.sub(r"^\\\s*\n+", "", new_cmd)
        # set -e; / set -o pipefail; / etc
        new_cmd = re.sub(r"^set\s+-\S+(\s+\S+)*\s*;\s*", "", new_cmd)
        # timeout NN
        new_cmd = re.sub(r"^timeout\s+\S+\s+", "", new_cmd)
        # cd X (followed by &&, ;, or newline)
        new_cmd = re.sub(r"^cd\s+\S+\s*(?:&&|;|\n)\s*", "", new_cmd)
        # env VAR=val ...
        new_cmd = re.sub(r"^env\s+(?:\w+=\S+\s+)+", "", new_cmd)
        # Bare VAR=val assignments (one or more), incl. VAR=$(...) and VAR="..."
        # Stops at the first token that isn't an assignment. Uses [\s\S] inside
        # $(...) and quoted strings so multiline values are handled. Trailing
        # separator can be whitespace or `;`.
        new_cmd = re.sub(
            r"""^(?:\w+=(?:"[^"]*"|'[^']*'|\$\([\s\S]*?\)|[^\s$"'(]\S*)[;\s]+)+""",
            "", new_cmd,
        )
        # echo "..." &&  (a "report header" before the real cmd)
        new_cmd = re.sub(
            r"""^echo\s+(?:"[^"]*"|'[^']*'|\S+)\s*&&\s*""",
            "", new_cmd,
        )
        new_cmd = new_cmd.lstrip()
        if new_cmd == cmd:
            break
        cmd = new_cmd

    # for/while/until ... do BODY; done — recurse into BODY
    m = re.match(
        r"^(?:for|while|until)\b.*?\bdo\s+(.+?)(?:;\s*done\b.*)?$",
        cmd, re.DOTALL,
    )
    if m:
        body = m.group(1).strip()
        if body and body != cmd:
            return _peel_bash(body)

    # if COND; then BODY; (else ...;)? fi — recurse into BODY
    m = re.match(
        r"^if\b.*?;\s*then\s+(.+?)(?:;\s*(?:elif|else|fi)\b.*)?$",
        cmd, re.DOTALL,
    )
    if m:
        body = m.group(1).strip()
        if body and body != cmd:
            return _peel_bash(body)

    # Split on first command separator (newline, |, ;, &&, ||).
    parts = re.split(r"[\|;\n]|&&|\|\|", cmd, maxsplit=1)
    return parts[0].strip()


def _classify_bash(cmd: str) -> str:
    """Classify a bash command by its primary purpose.

    Also handles English-prose descriptions emitted by async sub-agent
    progress events (e.g. "Check compilation", "List source files") by
    matching a lowercased leading verb against the same category sets."""
    # Write detection: redirect, tee, sed -i anywhere in the line.
    if (re.search(r"(^|\s)>>?\s", cmd)
            or re.search(r"\btee\s", cmd)
            or re.search(r"\bsed\s+-i\b", cmd)):
        return CAT_WRITE

    head = _peel_bash(cmd)
    if not head:
        return CAT_OTHER
    tokens = head.split()
    first = tokens[0] if tokens else ""
    first_lc = first.lower()

    if first.startswith(("./", "/", "../")):
        return CAT_BUILD
    # Lowercase comparison so English-verb prose ("Check ...", "List ...")
    # still matches. Real shell commands are already lowercase, so this
    # doesn't change their classification.
    if first_lc in _READ_CMDS:
        return CAT_READ
    if first_lc in _WRITE_CMDS:
        return CAT_WRITE
    if first_lc in _BUILD_CMDS:
        return CAT_BUILD
    return CAT_OTHER


def _classify_tool(tu: ToolUse) -> str:
    if tu.name in ("Read", "Glob", "Grep"):
        return CAT_READ
    if tu.name in ("Write", "Edit", "NotebookEdit"):
        return CAT_WRITE
    if tu.name == "Bash":
        return _classify_bash(tu.input.get("command", ""))
    if tu.name == "Agent":
        return CAT_SUBAGENT
    return CAT_OTHER


def _tool_size(tu: ToolUse) -> int:
    """Visualization weight for a tool use, in characters."""
    if tu.size_override is not None:
        return tu.size_override
    if tu.name == "Write":
        return len(tu.input.get("content", ""))
    if tu.name == "Edit":
        return len(tu.input.get("new_string", ""))
    # All others: characters that came back into context.
    return len(tu.result.content) if tu.result else 0


# Map from progress-event description prefix (per `last_tool_name`) to
# regex that strips the prefix to recover the closest thing to the original
# tool input. The prefix scheme is set by the framework, see how each tool
# emits task_progress descriptions: e.g. Read → "Reading <path>", Bash →
# "Running <agent-supplied-description>", etc.
_PROGRESS_PREFIX = {
    "Read":  re.compile(r"^Reading\s+", re.IGNORECASE),
    "Write": re.compile(r"^Writing\s+", re.IGNORECASE),
    "Edit":  re.compile(r"^Editing\s+", re.IGNORECASE),
    "Glob":  re.compile(r"^Finding\s+", re.IGNORECASE),
    "Grep":  re.compile(r"^Searching for\s+", re.IGNORECASE),
    "Bash":  re.compile(r"^Running\s+", re.IGNORECASE),
}


def _synthesize_async_subagent_turns(
    snapshots: list[ProgressSnapshot],
) -> list[Turn]:
    """Async sub-agents have no parented conversation records in the trace,
    only `task_progress` snapshots. Build one pseudo-Turn per snapshot, each
    holding a single ToolUse named after `last_tool_name`. Sizes use
    `total_tokens` deltas as a proxy for "context grown by this tool call".

    The synthesized turns are approximate — exact tool inputs/outputs are not
    in the trace — but they are enough for the visualizer to render the
    activity, and tooltips carry the framework-supplied description.

    Per-tool input population: framework progress descriptions follow stable
    prefix patterns ("Reading <path>", "Running <bash-desc>", ...). We strip
    the prefix and put the body into the field _classify_tool / _segment_turn
    expects (command for Bash, file_path for Read/Write/Edit, pattern for
    Glob/Grep), so downstream classifiers see something useful instead of an
    empty input dict.
    """
    turns: list[Turn] = []
    prev_tokens = 0
    for idx, snap in enumerate(snapshots):
        token_delta = max(snap.total_tokens - prev_tokens, 0)
        prev_tokens = snap.total_tokens
        tool_name = snap.last_tool_name or "Bash"
        # Strip the framework prefix to recover the body.
        body = snap.description or ""
        prefix_re = _PROGRESS_PREFIX.get(tool_name)
        if prefix_re is not None:
            body = prefix_re.sub("", body, count=1)
        # Populate the input field downstream code looks at, by tool kind.
        synthetic_input: dict = {"description": snap.description}
        if tool_name == "Bash":
            synthetic_input["command"] = body
        elif tool_name in ("Read", "Write", "Edit"):
            synthetic_input["file_path"] = body
        elif tool_name in ("Glob", "Grep"):
            synthetic_input["pattern"] = body
        tu = ToolUse(
            id=f"async-progress-{idx}",
            name=tool_name,
            input=synthetic_input,
            size_override=token_delta,
        )
        turn = Turn(turn_index=len(turns) + 1)
        turn.content_blocks.append(ContentBlock(type="tool_use", tool_use=tu))
        turns.append(turn)
    return turns


def _think_size(turn: Turn) -> int:
    return sum(
        len(b.thinking or "") if b.type == "thinking" else len(b.text or "")
        for b in turn.content_blocks
        if b.type in ("thinking", "text")
    )


@dataclass
class _Segment:
    category: str
    size: int
    tooltip: str

PREVIEW_SIZE = 400  # chars of tool input/result to show in tooltip

def _segment_turn(turn: Turn) -> list[_Segment]:
    """Break a turn into ordered segments; think first, then tool ops."""
    segs: list[_Segment] = []

    think_size = _think_size(turn)
    if think_size > 0:
        previews: list[str] = []
        for b in turn.content_blocks:
            if b.type == "thinking" and b.thinking:
                previews.append("💭 " + b.thinking[:200].replace("\n", " "))
            elif b.type == "text" and b.text:
                previews.append("💬 " + b.text[:200].replace("\n", " "))
        tip = "\n".join(previews[:4]) if previews else f"think ({think_size})"
        segs.append(_Segment(CAT_THINK, think_size, tip))

    for tu in turn.tool_uses:
        size = _tool_size(tu)
        if size <= 0:
            continue
        cat = _classify_tool(tu)
        # For synthesized async-sub-agent ToolUses the only input we have is
        # `description` (lifted from a task_progress snapshot). Use it as a
        # universal fallback when the usual fields aren't present.
        desc_fallback = tu.input.get("description", "") if isinstance(tu.input, dict) else ""
        if tu.name == "Bash":
            cmd = tu.input.get("command", "")
            tip = f"$ {cmd[:400]}" if cmd else f"Bash: {desc_fallback}"
        elif tu.name in ("Read", "Glob", "Grep"):
            target = (
                tu.input.get("file_path")
                or tu.input.get("pattern", "")
                or desc_fallback
            )
            tip = f"{tu.name}: {target}"
        elif tu.name in ("Write", "Edit"):
            target = tu.input.get("file_path", "") or desc_fallback
            tip = f"{tu.name}: {target}"
        elif tu.name == "Agent":
            desc = tu.input.get("description", "")
            prompt_preview = tu.input.get("prompt", "")[:PREVIEW_SIZE].replace("\n", " ")
            result_preview = (tu.result.content[:PREVIEW_SIZE] if tu.result else "").replace("\n", " ")
            tip = f"[task] {desc}\n[prompt] {prompt_preview}\n[result] {result_preview}"
        else:
            tip = f"{tu.name}: {desc_fallback}" if desc_fallback else tu.name
        segs.append(_Segment(cat, size, tip))

    return segs


@dataclass
class _Row:
    """One line in the SVG timeline. Either a turn (segments), a context
    compaction divider (compact set), a blank spacer (spacer=True), or a
    session summary banner (summary_text set).
    `height` overrides the default row height."""
    label: str
    depth: int
    segments: list[_Segment] = field(default_factory=list)
    compact: Optional[CompactEvent] = None
    spacer: bool = False
    height: Optional[int] = None
    summary_text: Optional[str] = None


@dataclass
class _Group:
    """A contiguous span of rows belonging to one sub-agent dispatch.
    Rendered as a thin vertical guide line in the indent gutter so siblings
    are visually grouped. Async sub-agents draw the line dashed because the
    row order is approximate (synthesized from progress snapshots)."""
    depth: int       # depth of the sub-agent's own rows (parent_depth + 1)
    start_idx: int   # first row index belonging to this sub-agent (inclusive)
    end_idx: int     # last row index (inclusive)
    tool_use: Optional[ToolUse] = None  # the parent's Agent ToolUse that spawned this group
    is_async: bool = False


def _flatten_rows(sessions: list[Session]) -> tuple[list[_Row], list[_Group]]:
    """
    Flatten sessions + sub-agents into renderable rows.

    Sub-agent turns are emitted immediately after the parent turn that
    spawned them, with depth = parent_depth + 1, so the SVG renderer can
    indent them visually. Top-level `compact_boundary` events are inserted
    between the turns they fall between.

    Also returns a list of `_Group` markers, one per sub-agent dispatch,
    so the renderer can draw a vertical guide line spanning each group.
    """
    rows: list[_Row] = []
    groups: list[_Group] = []

    def emit(turns: list[Turn], prefix: str, depth: int,
             compacts_by_after: dict[int, list[CompactEvent]]) -> None:
        # Compaction can occur before any turn was issued (rare).
        for ev in compacts_by_after.get(0, []):
            rows.append(_Row(label="", depth=depth, compact=ev))

        for t in turns:
            label = f"{prefix}T{t.turn_index}"
            rows.append(_Row(label=label, depth=depth, segments=_segment_turn(t)))
            # Number sub-agents within this turn so labels disambiguate them.
            # A turn can issue several Agent calls (true parallel via multiple
            # tool_use blocks in one message); without numbering, every
            # sub-agent's "T1" looks identical to every other sub-agent's "T1".
            sub_idx = 0
            sub_count = sum(
                1 for tu in t.tool_uses
                if tu.name == "Agent" and tu.subagent is not None
            )
            for tu in t.tool_uses:
                if tu.name == "Agent" and tu.subagent:
                    sub_idx += 1
                    sub_prefix = f"{prefix}T{t.turn_index}/A{sub_idx}:"
                    grp_start = len(rows)
                    # Sub-agent compactions are not currently surfaced.
                    emit(tu.subagent.conversation, sub_prefix, depth + 1, {})
                    grp_end = len(rows) - 1
                    if grp_end >= grp_start:
                        groups.append(_Group(
                            depth=depth + 1,
                            start_idx=grp_start, end_idx=grp_end,
                            tool_use=tu,
                            is_async=tu.subagent.is_async,
                        ))
                    # Insert a thin spacer between sibling sub-agents so the
                    # boundary between A{n} and A{n+1} is visually obvious
                    # without wasting a full row of vertical space.
                    if sub_idx < sub_count:
                        rows.append(_Row(
                            label="", depth=depth + 1, spacer=True, height=8,
                        ))
            for ev in compacts_by_after.get(t.turn_index, []):
                rows.append(_Row(label="", depth=depth, compact=ev))

    for s_idx, s in enumerate(sessions, 1):
        # Full-height blank row between sessions, so the visual transition
        # from translation → verification is unmistakable.
        if s_idx > 1:
            rows.append(_Row(label="", depth=0, spacer=True))
        prefix = f"S{s_idx}." if len(sessions) > 1 else ""
        # Bucket compacts by the main turn they follow.
        by_after: dict[int, list[CompactEvent]] = defaultdict(list)
        for ev in s.compact_events:
            by_after[ev.after_turn_index].append(ev)
        emit(s.conversation, prefix, 0, by_after)
        # Append a summary banner at the end of this session.
        rows.append(_Row(
            label="", depth=0, summary_text=_format_session_summary(s, s_idx),
        ))

    return rows, groups


def _count_subagents(turns: list[Turn]) -> tuple[int, int]:
    """Count (sync, async) sub-agents recursively across the turn tree."""
    sync = 0
    async_ = 0
    for t in turns:
        for blk in t.content_blocks:
            if blk.type == "tool_use" and blk.tool_use and blk.tool_use.subagent:
                sa = blk.tool_use.subagent
                if sa.is_async:
                    async_ += 1
                else:
                    sync += 1
                ns, na = _count_subagents(sa.conversation)
                sync += ns
                async_ += na
    return sync, async_


def _format_duration(ms: int) -> str:
    if ms <= 0: return "?"
    s = ms // 1000
    if s < 60: return f"{s}s"
    m, s = divmod(s, 60)
    if m < 60: return f"{m}m{s:02d}s"
    h, m = divmod(m, 60)
    return f"{h}h{m:02d}m"


def _format_compact_int(n: int) -> str:
    if n >= 1_000_000: return f"{n / 1_000_000:.1f}M"
    if n >= 1_000:     return f"{n / 1_000:.1f}k"
    return str(n)


def _format_session_summary(s: Session, s_idx: int) -> str:
    parts = [f"S{s_idx} {s.phase}:"]
    r = s.result
    # Use len(s.conversation) — the actual main-agent turn count we parsed.
    # `result.num_turns` is a framework-side counter that reports something
    # different (often only the final wrap-up turns), so don't trust it here.
    parts.append(f"{len(s.conversation)} turns")
    sync_n, async_n = _count_subagents(s.conversation)
    if sync_n or async_n:
        if async_n:
            parts.append(f"{sync_n + async_n} sub-agents ({async_n} async)")
        else:
            parts.append(f"{sync_n} sub-agents")
    if s.compact_events:
        parts.append(f"{len(s.compact_events)} compactions")
    # Token totals from result.model_usage (input + output across all models).
    if r is not None and r.model_usage:
        in_tok = sum(mu.input_tokens for mu in r.model_usage.values())
        out_tok = sum(mu.output_tokens for mu in r.model_usage.values())
        cache_r = sum(mu.cache_read_tokens for mu in r.model_usage.values())
        cache_c = sum(mu.cache_creation_tokens for mu in r.model_usage.values())
        if in_tok or out_tok or cache_r or cache_c:
            parts.append(
                f"{_format_compact_int(in_tok + out_tok)} new tok "
                f"(in={_format_compact_int(in_tok)} out={_format_compact_int(out_tok)} "
                f"cache_r={_format_compact_int(cache_r)} "
                f"cache_c={_format_compact_int(cache_c)})"
            )
    # Use the timestamp-derived wall clock — the only field that matches
    # actual elapsed time. Frame-side `duration_ms` is broken for async
    # sub-agents (only counts main-agent activity, missing idle wait), and
    # `duration_api_ms` double-counts parallel sub-agent work.
    # If parallel work compressed real time noticeably (api_ms > 1.3× wall),
    # also report api_ms as a secondary "parallel work" figure.
    if s.wall_clock_ms:
        parts.append(_format_duration(s.wall_clock_ms))
        if r is not None and r.duration_api_ms > s.wall_clock_ms * 13 // 10:
            parts.append(
                f"({_format_duration(r.duration_api_ms)} api, parallelized)"
            )
    elif r is not None and r.duration_ms:
        parts.append(_format_duration(r.duration_ms))
    if r is not None and r.total_cost_usd:
        parts.append(f"${r.total_cost_usd:.2f}")
    if r is not None and r.is_error:
        parts.append(f"⚠ stop={r.stop_reason}")
    return "   ".join(parts)


def render_timeline_svg(sessions: list[Session]) -> str:
    rows, groups = _flatten_rows(sessions)
    if not rows:
        return '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="20"/>'

    max_total = max(
        (sum(seg.size for seg in row.segments) for row in rows), default=1
    ) or 1
    max_depth = max((row.depth for row in rows), default=0)
    n_turn_rows = sum(1 for row in rows if row.compact is None and not row.spacer)

    row_h = 14
    label_w = 90
    bar_w = 1400
    indent_px = 24
    bar_inset = 8   # gap between the group guide line and the start of bars
    pad_top = 90
    pad_bottom = 30

    # Pre-compute the y-offset and height of each row, so the renderer can
    # support variable-height rows (e.g. thin sub-agent spacers).
    row_y: list[int] = []
    row_heights: list[int] = []
    cursor = pad_top
    for row in rows:
        row_y.append(cursor)
        h = row.height if row.height is not None else row_h
        row_heights.append(h)
        cursor += h
    total_rows_height = cursor - pad_top
    height = pad_top + total_rows_height + pad_bottom
    width = label_w + max_depth * indent_px + bar_inset + bar_w + 40

    out: list[str] = []
    out.append('<?xml version="1.0" encoding="UTF-8"?>')
    out.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" '
        f'font-family="ui-monospace, monospace" font-size="11">'
    )
    out.append(f'<rect width="{width}" height="{height}" fill="{_BG}"/>')

    title = f"Trace timeline — {n_turn_rows} turns"
    out.append(
        f'<text x="20" y="28" font-size="16" font-weight="bold" fill="{_TITLE}">'
        f'{html.escape(title)}</text>'
    )

    legend_x = 20
    for cat, color in CAT_COLORS.items():
        out.append(
            f'<rect x="{legend_x}" y="50" width="14" height="14" '
            f'fill="{color}"/>'
        )
        out.append(
            f'<text x="{legend_x + 20}" y="62" fill="{_TEXT}">{cat}</text>'
        )
        legend_x += 80

    # X-axis tick marks (rough scale guide). Anchored at where depth-0 bars
    # actually start (label_w + bar_inset), so the ticks line up with the
    # left edge of the bar area.
    tick_origin = label_w + bar_inset
    for frac in (0.25, 0.5, 0.75, 1.0):
        x = tick_origin + bar_w * frac
        out.append(
            f'<line x1="{x}" y1="{pad_top - 6}" x2="{x}" y2="{height - pad_bottom}" '
            f'stroke="{_GRID}" stroke-dasharray="2,3"/>'
        )
        out.append(
            f'<text x="{x}" y="{pad_top - 10}" text-anchor="middle" '
            f'fill="{_TEXT_DIM}" font-size="10">{int(max_total * frac):,}</text>'
        )

    compact_color = "#c878d8"  # magenta-ish; stands out on dark bg

    # Sub-agent group guide lines: a purple vertical stroke spanning all
    # rows that belong to one sub-agent dispatch. Sits in the indent gutter
    # at the column where bars would have started without `bar_inset`,
    # `bar_inset` pixels to the left of the actual bars. Hovering the line
    # surfaces the same tooltip as the parent's purple Agent bar segment.
    group_color = CAT_COLORS.get(CAT_SUBAGENT, "#9b80c8")
    for grp in groups:
        gx = label_w + grp.depth * indent_px + 3
        gy1 = row_y[grp.start_idx] + 1
        gy2 = row_y[grp.end_idx] + row_heights[grp.end_idx] - 1
        tooltip_lines = []
        if grp.tool_use is not None:
            tu = grp.tool_use
            size = _tool_size(tu)
            desc = tu.input.get("description", "") if isinstance(tu.input, dict) else ""
            prompt_preview = (
                tu.input.get("prompt", "")[:PREVIEW_SIZE].replace("\n", " ")
                if isinstance(tu.input, dict) else ""
            )
            result_preview = (
                tu.result.content[:PREVIEW_SIZE] if tu.result else ""
            ).replace("\n", " ")
            tooltip_lines.append(f"{CAT_SUBAGENT}: {size:,} chars")
            tooltip_lines.append(f"[task] {desc}")
            tooltip_lines.append(f"[prompt] {prompt_preview}")
            tooltip_lines.append(f"[result] {result_preview}")
        tooltip = html.escape("\n".join(tooltip_lines)) if tooltip_lines else ""
        # Async sub-agents: dashed line, signaling that the row order is
        # only approximate (synthesized from progress snapshots, not a true
        # API conversation).
        dash_attr = ' stroke-dasharray="5,3"' if grp.is_async else ''
        if tooltip:
            out.append(
                f'<g><title>{tooltip}</title>'
                f'<line x1="{gx}" y1="{gy1}" x2="{gx}" y2="{gy2}" '
                f'stroke="{group_color}" stroke-width="3"{dash_attr}/></g>'
            )
        else:
            out.append(
                f'<line x1="{gx}" y1="{gy1}" x2="{gx}" y2="{gy2}" '
                f'stroke="{group_color}" stroke-width="3"{dash_attr}/>'
            )

    for idx, row in enumerate(rows):
        y = row_y[idx]
        rh = row_heights[idx]

        # Spacer: blank vertical gap between row groups (sub-agent siblings
        # use a thin one; sessions use a full-row one). Draws nothing.
        if row.spacer:
            continue

        # Session summary banner: end-of-session stats (turns, sub-agents,
        # compactions, tokens, duration, cost). Spans the full width.
        if row.summary_text is not None:
            band_x = label_w
            band_w = width - band_x - 40
            out.append(
                f'<rect x="{band_x}" y="{y}" width="{band_w}" '
                f'height="{rh}" fill="#2a2f3a" stroke="{_GRID}" stroke-width="0.5"/>'
            )
            out.append(
                f'<text x="{band_x + 8}" y="{y + 11}" '
                f'fill="{_TITLE}" font-size="11" font-weight="bold">'
                f'{html.escape(row.summary_text)}</text>'
            )
            continue

        # Compaction divider: full-width band with a label.
        if row.compact is not None:
            ev = row.compact
            band_x = label_w
            band_w = width - band_x - 40
            cy = y + row_h / 2
            tooltip = html.escape(
                f"context compaction ({ev.trigger})\n"
                f"{ev.pre_tokens:,} → {ev.post_tokens:,} tokens "
                f"({100 * ev.post_tokens / max(ev.pre_tokens, 1):.0f}%)\n"
                f"duration: {ev.duration_ms / 1000:.1f}s"
            )
            label = (
                f"⌁ compact: {ev.pre_tokens // 1000}k → {ev.post_tokens // 1000}k tok "
                f"({ev.duration_ms / 1000:.0f}s, {ev.trigger})"
            )
            out.append(
                f'<g><title>{tooltip}</title>'
                f'<line x1="{band_x}" y1="{cy:.1f}" x2="{band_x + band_w}" y2="{cy:.1f}" '
                f'stroke="{compact_color}" stroke-width="1" stroke-dasharray="4,3"/>'
                f'<text x="{band_x + 10}" y="{cy + 4:.1f}" '
                f'fill="{compact_color}" font-size="10">{html.escape(label)}</text>'
                f'</g>'
            )
            continue

        # `gutter_x` is where the group guide line lives; bars start
        # `bar_inset` pixels further to the right.
        gutter_x = label_w + row.depth * indent_px
        bar_start = gutter_x + bar_inset
        if idx % 2 == 0:
            # Start the zebra stripe at bar_start (not gutter_x) so the
            # `bar_inset` gap stays clear and doesn't paint over the
            # purple group guide line.
            out.append(
                f'<rect x="{bar_start}" y="{y}" '
                f'width="{width - bar_start - 40}" '
                f'height="{row_h}" fill="{_ROW_ALT}"/>'
            )
        label_fill = _TEXT_DIM if row.depth > 0 else _TEXT
        out.append(
            f'<text x="{gutter_x - 5}" y="{y + 11}" text-anchor="end" '
            f'fill="{label_fill}">{html.escape(row.label)}</text>'
        )
        x = float(bar_start)
        seg_gap = 2.0  # px gap between adjacent segments so equal-color blocks read separately
        for i, seg in enumerate(row.segments):
            w = max(seg.size / max_total * bar_w, 0.5)
            color = CAT_COLORS.get(seg.category, "#999")
            tooltip = html.escape(
                f"{seg.category}: {seg.size:,} chars\n{seg.tooltip}"
            )
            out.append(
                f'<g><title>{tooltip}</title>'
                f'<rect x="{x:.2f}" y="{y + 1}" width="{w:.2f}" '
                f'height="{row_h - 2}" fill="{color}"/></g>'
            )
            x += w
            if i < len(row.segments) - 1:
                x += seg_gap

    out.append('</svg>')
    return "\n".join(out)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    args = sys.argv[1:]
    if not args:
        print("Usage: parse_trace.py <file> [-v] [-r] [-f] [--readable] [--visualize] [--file-io]")
        sys.exit(1)

    path = args[0]

    # Parse flags: -v, -r, -f, --readable, --visualize, --file-io
    flags: set[str] = set()
    for arg in args[1:]:
        if arg.startswith("--"):
            flags.add(arg)
        elif arg.startswith("-"):
            for ch in arg[1:]:
                flags.add(ch)

    readable = "r" in flags or "--readable" in flags
    visualize = "v" in flags or "--visualize" in flags
    file_io = "f" in flags or "--file-io" in flags

    parser = TraceParser()
    sessions = parser.parse_file(path)

    if readable:
        out_path = path.rsplit(".", 1)[0] + "_readable.txt"
        content = build_readable_history(sessions)
        with open(out_path, "w") as f:
            f.write(content)
        print(f"Written to {out_path}  ({len(content):,} chars)")

    if visualize:
        out_path = path.rsplit(".", 1)[0] + "_timeline.svg"
        content = render_timeline_svg(sessions)
        with open(out_path, "w") as f:
            f.write(content)
        print(f"Written to {out_path}  ({len(content):,} chars)")

    if file_io:
        import json as _json
        out_path = path.rsplit(".", 1)[0] + "_file_io.json"
        report = generate_file_io_report(sessions)
        with open(out_path, "w") as f:
            _json.dump(report, f, indent=2)
        print(f"Written to {out_path}")

    if not readable and not visualize and not file_io:
        print_session_stats(sessions)
