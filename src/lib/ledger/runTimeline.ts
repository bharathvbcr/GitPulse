/**
 * One agent run's activity timeline, projected from the ledger rows it left.
 *
 * A run is two session ids: the terminal's, which the spawn and lifecycle rows
 * carry, and the agent's own, which the transcript rows carry (the run record
 * bridges them: `session_id` and `id`). This turns the rows of both into what
 * happened (edits, commands and tests with how each ended and how long it took)
 * and how the run's time was spent (working, waiting on a tool, waiting for the
 * reader, idle, ended), plus the tokens it used.
 *
 * Everything here follows the delivery timeline's rule (`delivery/timeline.ts`):
 * every number can be absent, and absent is never zero. A span is measured only
 * between lifecycle rows that were both observed; the time before the first
 * one, and the time after the last one of a run that is not live and did not
 * end in a row, is `unknown`, which is not `idle`. A command whose result was
 * never read has no outcome and no duration, and says so.
 *
 * Pure, so it is tested without a webview.
 */
import { displayText, formatDuration } from "../delivery/timeline";
import { eventTimeMs, kindForAction } from "./projection";
import type { LedgerEvent } from "./types";

/**
 * The lifecycle events a `session.state` row records: the hook bridge's whole
 * vocabulary (`alerts::bridge::EVENTS`). Held to `ledger/lifecycle.rs` by
 * `runTimeline.contract.test.ts`, which also holds `stateFor` to `state_of`.
 */
export const SESSION_STATE_EVENTS = [
  "permission_prompt",
  "idle_prompt",
  "agent_needs_input",
  "agent_completed",
  "elicitation_dialog",
  "elicitation_url_dialog",
  "error",
  "permission_request",
  "turn_finished",
  "prompt_submitted",
  "tool_finished",
  "session_ended",
] as const;
export type SessionStateEvent = (typeof SESSION_STATE_EVENTS)[number];

/** What the run was doing. */
export type RunState = "working" | "waiting_tool" | "waiting_you" | "idle" | "ended";
/** A span's state: what the run was doing, or that it is not known. */
export type SegmentKind = RunState | "unknown";

/** The words the panel draws for each. */
export const SEGMENT_LABELS: Record<SegmentKind, string> = {
  working: "Working",
  waiting_tool: "Waiting on a tool",
  waiting_you: "Waiting for you",
  idle: "Idle",
  ended: "Ended",
  unknown: "Not observed",
};

/** What a lifecycle event says the run is doing from then on. */
export function stateFor(event: SessionStateEvent): RunState {
  switch (event) {
    case "prompt_submitted":
    case "tool_finished":
      return "working";
    case "turn_finished":
    case "agent_completed":
      return "idle";
    case "permission_request":
    case "permission_prompt":
    case "idle_prompt":
    case "agent_needs_input":
    case "elicitation_dialog":
    case "elicitation_url_dialog":
    case "error":
      return "waiting_you";
    case "session_ended":
      return "ended";
  }
}

export type ItemKind = "edit" | "command" | "test";
/** `unobserved`: the result was never read, which is not a pass. */
export type ItemOutcome = "ok" | "failed" | "unobserved";

export interface TimelineItem {
  id: number;
  kind: ItemKind;
  /** When it ran, epoch milliseconds; null when the row carries no usable time. */
  atMs: number | null;
  label: string;
  outcome: ItemOutcome;
  /** How long it took, from its call to its result; null when not measured. */
  durationMs: number | null;
  /** The subagent that did it, or null for the run's own agent. */
  subagentId: string | null;
}

export interface StateSegment {
  state: SegmentKind;
  startMs: number;
  /** Null when the span has no observed or stated end. */
  endMs: number | null;
  durationMs: number | null;
  /**
   * What ended the span: the next lifecycle row (`observed`), the present of a
   * live run (`now`), the run's recorded end (`finished`), or nothing known.
   */
  bound: "observed" | "now" | "finished" | "unknown";
  /** The lifecycle event the span started at, when it started at one. */
  cause: SessionStateEvent | null;
}

export interface TokenTotals {
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
  total: number;
  /** Responses counted. */
  responses: number;
}

export interface RunTimeline {
  items: TimelineItem[];
  segments: StateSegment[];
  /** Known time per state; a state with no known span is absent, not 0. */
  spentMs: Partial<Record<SegmentKind, number>>;
  /** Null when no response's usage was recorded, which is not zero tokens. */
  tokens: TokenTotals | null;
  /** The latest time any row of the run carries, or null. */
  lastActivityMs: number | null;
  /** Rows were read up to the limit, so older ones may be missing. */
  capped: boolean;
  /** Lifecycle rows read: none means the segments cannot be drawn. */
  lifecycleRows: number;
  /** Items and counts the reader sees at a glance. */
  failed: number;
  unobserved: number;
}

/** A run's own clock, as the run record states it (epoch seconds). */
export interface RunWindow {
  claimedAt?: number | null;
  startedAt?: number | null;
  finishedAt?: number | null;
  /** The run is starting or running, so its last span reaches the present. */
  live: boolean;
}

const TEST_COMMAND =
  /(^|[\s;&|(])(cargo\s+(nextest|test)|(npm|pnpm|yarn|bun)\s+(run\s+)?test|vitest|jest|pytest|go\s+test|test:browser)(\s|$)/;

/** Whether a shell command line runs a test suite. */
export function isTestCommand(command: string): boolean {
  return TEST_COMMAND.test(command);
}

function detailOf(event: LedgerEvent): Record<string, unknown> | null {
  if (!event.detail_json) return null;
  try {
    const parsed: unknown = JSON.parse(event.detail_json);
    return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function count(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

function itemOf(event: LedgerEvent, detail: Record<string, unknown> | null): TimelineItem {
  const label = displayText(event.object ?? event.action);
  const edit = kindForAction(event.action) === "edit";
  // The column cannot say "unknown", so a row is a pass or a failure only when
  // its detail says its result was read. A row from before results were paired
  // has no such word and is `unobserved` too.
  const observed = detail?.result === "ok" || detail?.result === "error";
  const outcome: ItemOutcome = !observed ? "unobserved" : event.outcome === "failed" ? "failed" : "ok";
  return {
    id: event.id,
    kind: edit ? "edit" : isTestCommand(event.object ?? "") ? "test" : "command",
    atMs: finiteTime(event),
    label,
    outcome,
    durationMs: observed ? count(event.duration_ms) : null,
    subagentId: typeof detail?.subagent_id === "string" ? detail.subagent_id : null,
  };
}

function finiteTime(event: LedgerEvent): number | null {
  const at = eventTimeMs(event);
  return Number.isFinite(at) ? at : null;
}

export interface Transition {
  atMs: number;
  event: SessionStateEvent;
  id: number;
}

function transitionOf(event: LedgerEvent, detail: Record<string, unknown> | null): Transition | null {
  const name = detail?.event;
  if (typeof name !== "string" || !(SESSION_STATE_EVENTS as readonly string[]).includes(name)) return null;
  const atMs = finiteTime(event);
  return atMs === null ? null : { atMs, event: name as SessionStateEvent, id: event.id };
}

function secondsToMs(seconds: number | null | undefined): number | null {
  return typeof seconds === "number" && Number.isFinite(seconds) && seconds > 0 ? seconds * 1000 : null;
}

/** Spans of a segment list, adjacent equal states joined. */
function joined(segments: StateSegment[]): StateSegment[] {
  const out: StateSegment[] = [];
  for (const segment of segments) {
    const last = out[out.length - 1];
    if (
      last &&
      last.state === segment.state &&
      last.endMs !== null &&
      last.endMs === segment.startMs &&
      segment.state !== "ended"
    ) {
      last.endMs = segment.endMs;
      last.durationMs = segment.endMs === null ? null : segment.endMs - last.startMs;
      last.bound = segment.bound;
      continue;
    }
    out.push({ ...segment });
  }
  return out;
}

/**
 * The state segments of a run from its lifecycle transitions.
 *
 * Each transition holds its state until the next. The time before the first
 * transition (back to the run's own start, when the record states one) is
 * `unknown`. The last transition's span reaches `now` for a live run and the
 * run's recorded end otherwise; a run that is neither, and did not end in a
 * row, has a last span whose end nobody observed, which is `unknown` and has no
 * duration.
 */
export function segmentsFrom(
  transitions: readonly Transition[],
  window: RunWindow,
  nowMs: number,
): StateSegment[] {
  const ordered = [...transitions].sort((a, b) => a.atMs - b.atMs || a.id - b.id);
  if (ordered.length === 0) return [];
  const out: StateSegment[] = [];
  const runStart = secondsToMs(window.startedAt) ?? secondsToMs(window.claimedAt);
  const first = ordered[0];
  if (runStart !== null && runStart < first.atMs) {
    out.push({
      state: "unknown",
      startMs: runStart,
      endMs: first.atMs,
      durationMs: first.atMs - runStart,
      bound: "observed",
      cause: null,
    });
  }
  const finishedMs = secondsToMs(window.finishedAt);
  ordered.forEach((transition, index) => {
    const state = stateFor(transition.event);
    const next = ordered[index + 1];
    if (state === "ended") {
      out.push({
        state,
        startMs: transition.atMs,
        endMs: null,
        durationMs: null,
        bound: "observed",
        cause: transition.event,
      });
      return;
    }
    if (next) {
      out.push({
        state,
        startMs: transition.atMs,
        endMs: next.atMs,
        durationMs: next.atMs - transition.atMs,
        bound: "observed",
        cause: transition.event,
      });
      return;
    }
    if (window.live && Number.isFinite(nowMs) && nowMs >= transition.atMs) {
      out.push({
        state,
        startMs: transition.atMs,
        endMs: nowMs,
        durationMs: nowMs - transition.atMs,
        bound: "now",
        cause: transition.event,
      });
    } else if (finishedMs !== null && finishedMs >= transition.atMs) {
      out.push({
        state,
        startMs: transition.atMs,
        endMs: finishedMs,
        durationMs: finishedMs - transition.atMs,
        bound: "finished",
        cause: transition.event,
      });
    } else {
      out.push({
        state: "unknown",
        startMs: transition.atMs,
        endMs: null,
        durationMs: null,
        bound: "unknown",
        cause: transition.event,
      });
    }
  });
  return joined(out);
}

/**
 * Splits the working spans where a command measurably ran: the run was then
 * waiting on that tool, not working. A command with no measured duration
 * leaves its span as it was, so unmeasured tool time stays `working`.
 */
export function withToolWaits(segments: StateSegment[], items: readonly TimelineItem[]): StateSegment[] {
  const waits = items
    .filter((item) => item.kind !== "edit" && item.atMs !== null && item.durationMs !== null && item.durationMs > 0)
    .map((item) => ({ from: item.atMs as number, to: (item.atMs as number) + (item.durationMs as number) }))
    .sort((a, b) => a.from - b.from);
  if (waits.length === 0) return segments;
  const out: StateSegment[] = [];
  for (const segment of segments) {
    if (segment.state !== "working" || segment.endMs === null) {
      out.push(segment);
      continue;
    }
    let cursor = segment.startMs;
    const end = segment.endMs;
    const piece = (state: SegmentKind, from: number, to: number, cause: SessionStateEvent | null) =>
      out.push({ state, startMs: from, endMs: to, durationMs: to - from, bound: "observed", cause });
    for (const wait of waits) {
      const from = Math.max(wait.from, cursor);
      const to = Math.min(wait.to, end);
      if (to <= from) continue;
      if (from > cursor) piece("working", cursor, from, cursor === segment.startMs ? segment.cause : null);
      piece("waiting_tool", from, to, null);
      cursor = to;
    }
    if (cursor === segment.startMs) {
      out.push(segment);
    } else if (cursor < end) {
      out.push({
        state: "working",
        startMs: cursor,
        endMs: end,
        durationMs: end - cursor,
        bound: segment.bound,
        cause: null,
      });
    } else {
      // The last piece reaches the span's end, so it takes the span's bound.
      out[out.length - 1].bound = segment.bound;
    }
  }
  return out;
}

/**
 * One run's timeline.
 *
 * `rows` is every row read for the run's session ids; `rowLimit` is the limit
 * they were read with, so a full read is reported as capped and not as
 * complete.
 */
export function projectRunTimeline(
  rows: readonly LedgerEvent[],
  window: RunWindow,
  nowMs: number,
  rowLimit: number,
): RunTimeline {
  const seen = new Set<number>();
  const items: TimelineItem[] = [];
  const transitions: Transition[] = [];
  let input = 0;
  let output = 0;
  let cacheRead = 0;
  let cacheCreation = 0;
  let responses = 0;
  let last: number | null = null;
  const touch = (at: number | null) => {
    if (at !== null && (last === null || at > last)) last = at;
  };
  for (const row of rows) {
    if (seen.has(row.id)) continue;
    seen.add(row.id);
    const detail = detailOf(row);
    switch (row.action) {
      case "session.edit":
      case "session.command": {
        const item = itemOf(row, detail);
        items.push(item);
        touch(item.atMs);
        break;
      }
      case "session.state": {
        const transition = transitionOf(row, detail);
        if (transition) {
          transitions.push(transition);
          touch(transition.atMs);
        }
        break;
      }
      case "session.usage": {
        const usage = detail?.usage;
        if (usage === null || typeof usage !== "object") break;
        const counts = usage as Record<string, unknown>;
        const parts = [counts.input, counts.output, counts.cache_read, counts.cache_creation].map(count);
        // A response whose counts are not all numbers is not summed in part.
        if (parts.some((part) => part === null)) break;
        input += parts[0] as number;
        output += parts[1] as number;
        cacheRead += parts[2] as number;
        cacheCreation += parts[3] as number;
        responses += 1;
        touch(finiteTime(row));
        break;
      }
      default:
        break;
    }
  }
  items.sort((a, b) => (a.atMs ?? Number.POSITIVE_INFINITY) - (b.atMs ?? Number.POSITIVE_INFINITY) || a.id - b.id);
  const segments = withToolWaits(segmentsFrom(transitions, window, nowMs), items);
  const spentMs: Partial<Record<SegmentKind, number>> = {};
  for (const segment of segments) {
    if (segment.durationMs === null) continue;
    spentMs[segment.state] = (spentMs[segment.state] ?? 0) + segment.durationMs;
  }
  return {
    items,
    segments,
    spentMs,
    tokens:
      responses === 0
        ? null
        : {
            input,
            output,
            cacheRead,
            cacheCreation,
            total: input + output + cacheRead + cacheCreation,
            responses,
          },
    lastActivityMs: last,
    capped: rows.length >= rowLimit,
    lifecycleRows: transitions.length,
    failed: items.filter((item) => item.outcome === "failed").length,
    unobserved: items.filter((item) => item.outcome === "unobserved").length,
  };
}

/** A token count for humans: `842`, `12.4k`, `3.1M`. */
export function formatTokens(count: number): string {
  if (!Number.isFinite(count) || count < 0) return "—";
  if (count < 1000) return String(Math.round(count));
  if (count < 1_000_000) return `${(count / 1000).toFixed(1)}k`;
  return `${(count / 1_000_000).toFixed(1)}M`;
}

/** The share of the known time each state took, for drawing a single bar. */
export function segmentShares(segments: readonly StateSegment[]): { state: SegmentKind; pct: number }[] {
  const known = segments.filter((segment) => segment.durationMs !== null && segment.state !== "ended");
  const total = known.reduce((sum, segment) => sum + (segment.durationMs as number), 0);
  if (total <= 0) return [];
  return known.map((segment) => ({ state: segment.state, pct: ((segment.durationMs as number) / total) * 100 }));
}

/** `1h 12m working · 3m 05s waiting on a tool`, known states only. */
export function spentSummary(spentMs: Partial<Record<SegmentKind, number>>): string {
  const order: SegmentKind[] = ["working", "waiting_tool", "waiting_you", "idle", "unknown"];
  return order
    .filter((state) => spentMs[state] !== undefined)
    .map((state) => `${formatDuration(spentMs[state] ?? null)} ${SEGMENT_LABELS[state].toLowerCase()}`)
    .join(" · ");
}

/** The instant a segment starts, as an ISO string, for a `title`. */
export function segmentTitle(segment: StateSegment): string {
  const start = new Date(segment.startMs).toISOString();
  const spent = segment.durationMs === null ? "length not known" : formatDuration(segment.durationMs);
  const tail = segment.bound === "now" ? " so far" : "";
  return `${SEGMENT_LABELS[segment.state]} from ${start}, ${spent}${tail}`;
}

