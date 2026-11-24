/**
 * Folds Claude Code's stream-json lines (and the host's `mavi_*` markers) into
 * a {@link ThreadState}. Mutates the state it is given, so a Svelte `$state`
 * proxy updates only what a line touched.
 *
 * What the wire does, as observed from Claude Code 2.1.330 (fixtures in
 * `__fixtures__/`):
 * - `stream_event` carries Anthropic deltas. Each content block also arrives
 *   whole as its own `assistant` line, several per API message id, each
 *   repeating usage. A replay (deltas are not kept) sees only those.
 * - A permission request (`control_request/can_use_tool`) arrives after its
 *   complete `tool_use`; it attaches to that tool by `tool_use_id`.
 * - `system/init` repeats every turn; it updates model and mode, nothing else.
 * - `post_turn_summary` arrives just before its `result`.
 * - `result.total_cost_usd` is cumulative; a turn's cost is the difference.
 * - Unknown line types are skipped, never an error.
 */
import type {
  AssistantItem,
  MaviHunk,
  ThinkingItem,
  ThreadState,
  TimelineItem,
  ToolItem,
  TurnEndItem,
  TurnSummary,
} from "./types";

export const MAX_ITEMS = 4000;
const MAX_STDERR = 200;
const MAX_RESULT_CHARS = 16_000;

export function emptyThread(): ThreadState {
  return {
    items: [],
    turn: 0,
    conversationId: null,
    model: null,
    permissionMode: null,
    cwd: null,
    agentVersion: null,
    activity: null,
    costUsd: 0,
    backgroundTasks: [],
    turnFiles: {},
    stderr: [],
    ended: null,
    lastSeq: 0,
  };
}

type Json = Record<string, unknown>;

function obj(value: unknown): Json | null {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Json) : null;
}
function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}
function num(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
function arr(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

/** Per-thread scratch the reducer needs but components never read. */
interface Scratch {
  /**
   * The message being streamed, per agent: the main agent under "" and each
   * subagent under its Task call's id, so their interleaved deltas never
   * write into one another's blocks.
   */
  streams: Map<string, Stream>;
  /** A `post_turn_summary` waiting for its `result`. */
  summary: TurnSummary | null;
  /** The cumulative cost at the end of the previous turn. */
  costBefore: number;
  /** Next item id number: ids never repeat, even once old items are dropped. */
  nextId: number;
  /**
   * Turns of earlier host sessions of this thread. A new session numbers its
   * turns from 1 again; this keeps them after the ones already shown.
   */
  turnBase: number;
}

interface Stream {
  /** The API message id the current deltas belong to. */
  messageId: string | null;
  /** Delta block index → item id, for the current message. */
  blocks: Map<number, string>;
}

const scratch = new WeakMap<ThreadState, Scratch>();

function streamOf(s: Scratch, parent: string | null): Stream {
  const key = parent ?? "";
  let stream = s.streams.get(key);
  if (!stream) {
    stream = { messageId: null, blocks: new Map() };
    s.streams.set(key, stream);
  }
  return stream;
}

function scratchOf(state: ThreadState): Scratch {
  let s = scratch.get(state);
  if (!s) {
    s = { streams: new Map(), summary: null, costBefore: state.costUsd, nextId: state.items.length, turnBase: 0 };
    scratch.set(state, s);
  }
  return s;
}

/** Items dropped at once past the cap, so a long thread does not reindex per line. */
const TRIM_CHUNK = 200;

function push(state: ThreadState, item: TimelineItem): void {
  state.items.push(item);
  if (state.items.length > MAX_ITEMS) state.items.splice(0, state.items.length - MAX_ITEMS + TRIM_CHUNK);
}

/** A number for an item id that is unique within the thread. */
function uid(state: ThreadState): number {
  const s = scratchOf(state);
  s.nextId += 1;
  return s.nextId;
}

/**
 * The thread will be followed from a new host session (a restart without a
 * saved conversation): its turn numbers and cost restart, so this run's turns
 * are kept and the new ones are numbered after them.
 */
export function continueInNewSession(state: ThreadState): void {
  const s = scratchOf(state);
  s.turnBase = state.turn;
  s.costBefore = 0;
  s.streams.clear();
  s.summary = null;
  state.lastSeq = 0;
  state.ended = null;
}

function find<T extends TimelineItem["kind"]>(
  state: ThreadState,
  kind: T,
  id: string,
): Extract<TimelineItem, { kind: T }> | undefined {
  for (let i = state.items.length - 1; i >= 0; i--) {
    const item = state.items[i]!;
    if (item.kind === kind && item.id === id) return item as Extract<TimelineItem, { kind: T }>;
  }
  return undefined;
}

function toolById(state: ThreadState, id: string): ToolItem | undefined {
  return find(state, "tool", id);
}

function newTool(state: ThreadState, id: string, name: string, input: Json, parentId: string | null): ToolItem {
  const tool: ToolItem = {
    kind: "tool",
    id,
    turn: state.turn,
    name,
    input,
    partialInput: "",
    status: "running",
    result: "",
    patch: null,
    parentId,
    approval: null,
    background: false,
  };
  push(state, tool);
  return tool;
}

function resultText(content: unknown): string {
  if (typeof content === "string") return content;
  return arr(content)
    .map((part) => str(obj(part)?.text) ?? (obj(part)?.type === "image" ? "[image]" : ""))
    .filter(Boolean)
    .join("\n");
}

function hunks(value: unknown): MaviHunk[] | null {
  const list = arr(value)
    .map((h) => obj(h))
    .filter((h): h is Json => !!h)
    .map((h) => ({
      oldStart: num(h.oldStart) ?? 0,
      oldLines: num(h.oldLines) ?? 0,
      newStart: num(h.newStart) ?? 0,
      newLines: num(h.newLines) ?? 0,
      lines: arr(h.lines).filter((l): l is string => typeof l === "string"),
    }));
  return list.length ? list : null;
}

/** The text and thinking items of one API message, in arrival order. */
function unfinished<K extends "assistant" | "thinking">(
  state: ThreadState,
  kind: K,
  messageId: string,
): Extract<TimelineItem, { kind: K }> | undefined {
  // Backward over the current turn only, keeping the earliest match: a
  // message's blocks finish in the order they started.
  let found: Extract<TimelineItem, { kind: K }> | undefined;
  for (let i = state.items.length - 1; i >= 0; i--) {
    const item = state.items[i]!;
    if (item.turn < state.turn) break;
    if (item.kind === kind && (item as AssistantItem | ThinkingItem).messageId === messageId && !(item as AssistantItem).final) {
      found = item as Extract<TimelineItem, { kind: K }>;
    }
  }
  return found;
}

function onStreamEvent(state: ThreadState, line: Json, s: Scratch): void {
  const event = obj(line.event);
  if (!event) return;
  const parent = str(line.parent_tool_use_id);
  const stream = streamOf(s, parent);
  switch (event.type) {
    case "message_start": {
      stream.messageId = str(obj(event.message)?.id) ?? `m${uid(state)}`;
      stream.blocks.clear();
      break;
    }
    case "content_block_start": {
      const index = num(event.index) ?? 0;
      const block = obj(event.content_block);
      const messageId = stream.messageId ?? "m";
      if (!block) break;
      const id = `${messageId}:${index}`;
      // A subagent's prose and thinking belong to its Task, whose result
      // reports what it found; only its tool calls are tracked here.
      if (parent && block.type !== "tool_use") break;
      if (block.type === "text") {
        push(state, { kind: "assistant", id, turn: state.turn, messageId, text: str(block.text) ?? "", streaming: true, final: false });
        stream.blocks.set(index, id);
      } else if (block.type === "thinking") {
        push(state, { kind: "thinking", id, turn: state.turn, messageId, text: "", streaming: true, final: false });
        stream.blocks.set(index, id);
      } else if (block.type === "tool_use") {
        const toolId = str(block.id) ?? id;
        if (!toolById(state, toolId)) newTool(state, toolId, str(block.name) ?? "tool", {}, parent);
        stream.blocks.set(index, toolId);
      }
      break;
    }
    case "content_block_delta": {
      const id = stream.blocks.get(num(event.index) ?? -1);
      const delta = obj(event.delta);
      if (!id || !delta) break;
      if (delta.type === "text_delta") {
        const item = find(state, "assistant", id);
        if (item && !item.final) item.text += str(delta.text) ?? "";
      } else if (delta.type === "thinking_delta") {
        const item = find(state, "thinking", id);
        if (item && !item.final) item.text += str(delta.thinking) ?? "";
      } else if (delta.type === "input_json_delta") {
        const tool = toolById(state, id);
        if (tool && Object.keys(tool.input).length === 0) tool.partialInput += str(delta.partial_json) ?? "";
      }
      break;
    }
    case "content_block_stop": {
      const id = stream.blocks.get(num(event.index) ?? -1);
      if (!id) break;
      const item = find(state, "assistant", id) ?? find(state, "thinking", id);
      if (item) item.streaming = false;
      break;
    }
    default:
      break;
  }
}

function onAssistant(state: ThreadState, line: Json): void {
  const message = obj(line.message);
  if (!message) return;
  const messageId = str(message.id) ?? `m${uid(state)}`;
  const parent = str(line.parent_tool_use_id);
  for (const raw of arr(message.content)) {
    const block = obj(raw);
    if (!block) continue;
    // A subagent's prose stays with its Task (see onStreamEvent).
    if (parent && block.type !== "tool_use") continue;
    if (block.type === "text") {
      const text = str(block.text) ?? "";
      const open = unfinished(state, "assistant", messageId);
      if (open) {
        open.text = text;
        open.streaming = false;
        open.final = true;
      } else if (text.trim()) {
        push(state, { kind: "assistant", id: `${messageId}:t${uid(state)}`, turn: state.turn, messageId, text, streaming: false, final: true });
      }
    } else if (block.type === "thinking" || block.type === "redacted_thinking") {
      const text = str(block.thinking) ?? "";
      const open = unfinished(state, "thinking", messageId);
      if (open) {
        if (text) open.text = text;
        open.streaming = false;
        open.final = true;
      } else {
        push(state, { kind: "thinking", id: `${messageId}:h${uid(state)}`, turn: state.turn, messageId, text, streaming: false, final: true });
      }
    } else if (block.type === "tool_use") {
      const id = str(block.id);
      if (!id) continue;
      const input = obj(block.input) ?? {};
      const tool = toolById(state, id) ?? newTool(state, id, str(block.name) ?? "tool", input, parent);
      tool.name = str(block.name) ?? tool.name;
      tool.input = input;
      tool.partialInput = "";
    }
  }
}

function onUser(state: ThreadState, line: Json): void {
  const message = obj(line.message);
  const meta = obj(line.tool_use_result);
  for (const raw of arr(message?.content)) {
    const block = obj(raw);
    if (!block) continue;
    if (block.type === "tool_result") {
      const id = str(block.tool_use_id);
      if (!id) continue;
      const tool = toolById(state, id) ?? newTool(state, id, "tool", {}, str(line.parent_tool_use_id));
      const text = resultText(block.content);
      tool.result = text.length > MAX_RESULT_CHARS ? `${text.slice(0, MAX_RESULT_CHARS)}\n…` : text;
      const isError = block.is_error === true;
      if (!isError) tool.status = "ok";
      else if (tool.approval?.state === "denied") tool.status = "denied";
      else if (/interrupted/i.test(text) && text.length < 200) tool.status = "interrupted";
      else tool.status = "error";
      // A tool that already ran (or was refused elsewhere) asks nothing more.
      if (tool.approval?.state === "open") tool.approval.state = "void";
      if (meta) {
        const patch = hunks(meta.structuredPatch);
        if (patch) tool.patch = patch;
        if (str(meta.backgroundTaskId)) tool.background = true;
      }
    } else if (block.type === "text") {
      const text = str(block.text) ?? "";
      if (/^\[Request interrupted/.test(text)) {
        push(state, { kind: "notice", id: `n${uid(state)}`, turn: state.turn, tone: "info", text: "You stopped this turn." });
      }
    }
  }
}

function onControlRequest(state: ThreadState, line: Json): void {
  const request = obj(line.request);
  const requestId = str(line.request_id);
  if (!request || request.subtype !== "can_use_tool" || !requestId) return;
  const toolId = str(request.tool_use_id) ?? `req:${requestId}`;
  const sent = obj(request.input) ?? {};
  // The host withholds an input too large (or unsafe) to show; it still
  // answers with the agent's real input.
  const hidden = typeof sent.mavi_input_hidden === "string";
  const input = hidden ? {} : sent;
  const tool = toolById(state, toolId) ?? newTool(state, toolId, str(request.tool_name) ?? "tool", input, null);
  if (Object.keys(tool.input).length === 0) tool.input = input;
  tool.approval = {
    requestId,
    state: "open",
    reason: str(request.description) ?? str(request.decision_reason),
    ...(hidden ? { hiddenBytes: num(sent.bytes) ?? 0 } : {}),
  };
}

function onControlCancel(state: ThreadState, line: Json): void {
  const requestId = str(line.request_id);
  if (!requestId) return;
  for (let i = state.items.length - 1; i >= 0; i--) {
    const item = state.items[i]!;
    if (item.kind === "tool" && item.approval?.requestId === requestId && item.approval.state === "open") {
      item.approval.state = "void";
      return;
    }
  }
}

function onResult(state: ThreadState, line: Json, s: Scratch): void {
  const subtype = str(line.subtype) ?? "unknown";
  const total = num(line.total_cost_usd);
  const cost = total === null ? null : Math.max(0, total - s.costBefore);
  if (total !== null) {
    state.costUsd = total;
    s.costBefore = total;
  }
  const interrupted = subtype === "error_during_execution";
  for (const item of state.items) {
    if (item.turn !== state.turn) continue;
    if (item.kind === "tool") {
      if (item.status === "running" && !item.background) item.status = "interrupted";
      if (item.approval?.state === "open") item.approval.state = "void";
    } else if (item.kind === "assistant" || item.kind === "thinking") {
      item.streaming = false;
    }
  }
  const end: TurnEndItem = {
    kind: "turn_end",
    id: `end:${state.turn}:${uid(state)}`,
    turn: state.turn,
    subtype,
    ok: subtype === "success" && line.is_error !== true,
    interrupted,
    durationMs: num(line.duration_ms),
    costUsd: cost,
    summary: s.summary,
    history: line.mavi_history === true,
  };
  s.summary = null;
  state.activity = null;
  push(state, end);
}

function onSystem(state: ThreadState, line: Json, s: Scratch): void {
  switch (line.subtype) {
    case "init":
      state.conversationId = str(line.session_id) ?? state.conversationId;
      state.model = str(line.model) ?? state.model;
      state.permissionMode = str(line.permissionMode) ?? state.permissionMode;
      state.cwd = str(line.cwd) ?? state.cwd;
      state.agentVersion = str(line.claude_code_version) ?? state.agentVersion;
      break;
    case "task_summary":
      state.activity = str(line.detail);
      break;
    case "post_turn_summary": {
      const summary: TurnSummary = {
        category: str(line.status_category) ?? "",
        detail: str(line.status_detail) ?? "",
        needsAction: str(line.needs_action) ?? "",
      };
      // Normally before its result; attach late if the result came first.
      const last = state.items[state.items.length - 1];
      if (last?.kind === "turn_end" && last.turn === state.turn && !last.summary) last.summary = summary;
      else s.summary = summary;
      break;
    }
    case "background_tasks_changed":
      state.backgroundTasks = arr(line.tasks)
        .map((t) => str(obj(t)?.description) ?? str(obj(t)?.task_id) ?? "")
        .filter(Boolean);
      break;
    case "hook_response":
      if (line.outcome && line.outcome !== "success") {
        push(state, {
          kind: "notice",
          id: `n${uid(state)}`,
          turn: state.turn,
          tone: "warn",
          text: `Hook ${str(line.hook_name) ?? ""} did not succeed${str(line.stderr) ? `: ${str(line.stderr)!.slice(0, 300)}` : "."}`,
        });
      }
      break;
    case "compact_boundary":
      push(state, { kind: "notice", id: `n${uid(state)}`, turn: state.turn, tone: "info", text: "The agent compacted its context." });
      break;
    default:
      break;
  }
}

function onMavi(state: ThreadState, line: Json): void {
  switch (line.type) {
    case "mavi_user": {
      const sent = num(line.turn);
      const turn = sent === null ? state.turn + 1 : sent + scratchOf(state).turnBase;
      state.turn = Math.max(state.turn, turn);
      if (line.automatic === true) {
        // A turn Claude Code started itself (a background task reporting
        // back): not the person's words, so not a message bubble.
        push(state, { kind: "notice", id: `auto:${turn}:${uid(state)}`, turn, tone: "info", text: "A background task reported back, and the agent continued." });
        break;
      }
      push(state, { kind: "user", id: `u${turn}:${uid(state)}`, turn, text: str(line.text) ?? "", at: num(line.at) });
      if (line.baseline === "unavailable") {
        push(state, { kind: "notice", id: `n${uid(state)}`, turn, tone: "warn", text: "Couldn't read the worktree's state before this turn, so changes made outside the agent's edit tools will not be listed." });
      }
      break;
    }
    case "mavi_answered": {
      const requestId = str(line.request_id);
      for (const item of state.items) {
        if (item.kind === "tool" && item.approval?.requestId === requestId) {
          item.approval.state = line.decision === "allow" ? "allowed" : "denied";
        }
      }
      break;
    }
    case "mavi_interrupt":
      for (const item of state.items) {
        if (item.kind === "tool" && item.approval?.state === "open") item.approval.state = "void";
      }
      break;
    case "mavi_turn_files":
      state.turnFiles[num(line.turn) === null ? state.turn : num(line.turn)! + scratchOf(state).turnBase] = {
        available: line.available === true,
        partial: line.partial === true,
        moved: arr(line.moved)
          .map((m) => obj(m))
          .filter((m): m is Json => !!m && typeof m.path === "string")
          .map((m) => ({ path: m.path as string, code: str(m.code) ?? "" })),
        reason: str(line.reason),
      };
      break;
    case "mavi_stderr": {
      const text = str(line.text);
      if (text) {
        state.stderr.push(text);
        if (state.stderr.length > MAX_STDERR) state.stderr.shift();
      }
      break;
    }
    case "mavi_exit": {
      state.ended = str(line.exit) ?? "ended";
      state.activity = null;
      for (const item of state.items) {
        if (item.kind === "tool" && item.approval?.state === "open") item.approval.state = "void";
        if (item.kind === "tool" && item.status === "running" && !item.background) item.status = "interrupted";
        if (item.kind === "assistant" || item.kind === "thinking") item.streaming = false;
      }
      const clean = line.ok === true;
      const tail = arr(line.stderr).filter((l): l is string => typeof l === "string").join("\n");
      push(state, {
        kind: "notice",
        id: `n${uid(state)}`,
        turn: state.turn,
        tone: clean ? "info" : "error",
        text: clean ? "The agent ended." : `The agent ${state.ended}.${tail ? `\n${tail}` : ""}`,
      });
      break;
    }
    case "mavi_history": {
      if (line.available === true) {
        const turns = num(line.turns) ?? 0;
        if (turns > 0) {
          push(state, {
            kind: "notice",
            id: `history:${uid(state)}`,
            turn: 0,
            tone: "info",
            text: `Earlier in this conversation: ${turns === 1 ? "1 turn" : `${turns} turns`} from Claude Code's transcript${line.truncated === true ? ", the oldest not shown" : ""}.`,
          });
        }
      } else {
        push(state, {
          kind: "notice",
          id: `history:${uid(state)}`,
          turn: state.turn,
          tone: "info",
          text: `Earlier messages are not shown here. ${str(line.reason) ?? ""}`.trim(),
        });
      }
      break;
    }
    case "mavi_history_end":
      if ((num(line.turns) ?? 0) > 0) {
        push(state, { kind: "notice", id: `history-end:${uid(state)}`, turn: state.turn, tone: "info", text: "Continues in Mavi from here." });
      }
      break;
    case "mavi_truncated":
      push(state, { kind: "notice", id: `n${uid(state)}`, turn: state.turn, tone: "warn", text: `A ${num(line.bytes) ?? "very large"}-byte line from the agent was too long to show.` });
      break;
    case "mavi_redacted":
      push(state, { kind: "notice", id: `n${uid(state)}`, turn: state.turn, tone: "info", text: "A line held a credential and was withheld." });
      break;
    default:
      break;
  }
}

/** Applies one line. Lines at or below `lastSeq` are skipped (replay overlap). */
export function applyLine(state: ThreadState, line: string, seq = 0): void {
  if (seq && seq <= state.lastSeq) return;
  if (seq) state.lastSeq = seq;
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    // Not JSON: something printed straight to stdout (a hook, a shell rc).
    if (line.trim()) {
      state.stderr.push(line.slice(0, 2000));
      if (state.stderr.length > MAX_STDERR) state.stderr.shift();
    }
    return;
  }
  const json = obj(value);
  if (!json) return;
  const s = scratchOf(state);
  const type = str(json.type) ?? "";
  if (type.startsWith("mavi_")) return onMavi(state, json);
  switch (type) {
    case "stream_event":
      return onStreamEvent(state, json, s);
    case "assistant":
      return onAssistant(state, json);
    case "user":
      return onUser(state, json);
    case "control_request":
      return onControlRequest(state, json);
    case "control_cancel_request":
      return onControlCancel(state, json);
    case "result":
      return onResult(state, json, s);
    case "system":
      return onSystem(state, json, s);
    case "rate_limit_event": {
      const status = str(obj(json.rate_limit_info)?.status);
      if (status && status !== "allowed" && status !== "allowed_warning") {
        push(state, { kind: "notice", id: `n${uid(state)}`, turn: state.turn, tone: "warn", text: "The agent hit a usage limit." });
      }
      return;
    }
    default:
      return;
  }
}

export function applyAll(state: ThreadState, lines: Iterable<{ seq: number; line: string } | string>): ThreadState {
  for (const entry of lines) {
    if (typeof entry === "string") applyLine(state, entry);
    else applyLine(state, entry.line, entry.seq);
  }
  return state;
}

/** Open permission requests, oldest first. */
export function openApprovals(state: ThreadState): ToolItem[] {
  return state.items.filter((i): i is ToolItem => i.kind === "tool" && i.approval?.state === "open");
}
