import { describe, expect, it } from "vitest";
import {
  formatTokens,
  isTestCommand,
  projectRunTimeline,
  segmentShares,
  spentSummary,
  stateFor,
  type RunWindow,
} from "./runTimeline";
import type { LedgerEvent } from "./types";

let nextId = 1;

function row(action: string, detail: Record<string, unknown> | null, overrides: Partial<LedgerEvent> = {}): LedgerEvent {
  const id = nextId++;
  return {
    id,
    ulid: `01M1F8Q43R3S5XG2A2${String(id).padStart(8, "0")}`,
    ts_utc: "2026-09-01T12:00:00.000Z",
    schema_version: 1,
    repo_path: "/repo",
    worktree_path: null,
    actor_kind: "agent",
    actor_id: "claude-code",
    session_id: "S1",
    task_id: null,
    action,
    object: null,
    argv_json: null,
    outcome: "ok",
    verdict_json: null,
    before_ref: null,
    after_ref: null,
    duration_ms: null,
    detail_json: detail === null ? null : JSON.stringify(detail),
    ...overrides,
  };
}

const T = (s: number) => Date.parse("2026-09-01T12:00:00.000Z") + s * 1000;
const at = (s: number) => new Date(T(s)).toISOString();

function state(event: string, seconds: number) {
  return row("session.state", { event, occurred_at: at(seconds) });
}

function command(object: string, seconds: number, extra: Partial<LedgerEvent> = {}, detail: Record<string, unknown> = {}) {
  return row("session.command", { occurred_at: at(seconds), ...detail }, { object, ...extra });
}

const LIVE: RunWindow = { live: true };
const GONE: RunWindow = { live: false };
const NOW = T(1000);

describe("stateFor", () => {
  it("maps every lifecycle event to what the run is doing", () => {
    expect(stateFor("prompt_submitted")).toBe("working");
    expect(stateFor("tool_finished")).toBe("working");
    expect(stateFor("turn_finished")).toBe("idle");
    expect(stateFor("permission_request")).toBe("waiting_you");
    expect(stateFor("idle_prompt")).toBe("waiting_you");
    expect(stateFor("error")).toBe("waiting_you");
    expect(stateFor("session_ended")).toBe("ended");
  });
});

describe("isTestCommand", () => {
  it("recognises the suites this repository runs, and not a lookalike", () => {
    for (const cmd of ["cargo test --lib", "npm run test", "npx vitest run", "pytest -q", "go test ./...", "cargo nextest run", "npm --prefix x run test:browser"]) {
      expect(isTestCommand(cmd), cmd).toBe(true);
    }
    for (const cmd of ["cargo build", "git log --grep test", "echo testing", "ls tests"]) {
      expect(isTestCommand(cmd), cmd).toBe(false);
    }
  });
});

describe("items", () => {
  it("marks a command by what its result said, and a missing result as unobserved", () => {
    const rows = [
      command("cargo build", 1, { duration_ms: 4200 }, { result: "ok" }),
      command("cargo test --lib", 10, { outcome: "failed", duration_ms: 90_000 }, { result: "error", subagent_id: "a1b2" }),
      // A row from before results were paired: no detail.result, column says ok.
      command("ls", 20),
      // The writer forces `ok` into the column for a call whose result was never read.
      command("sleep 600", 30, {}, { result: "unobserved" }),
    ];
    const timeline = projectRunTimeline(rows, GONE, NOW, 100);
    expect(timeline.items.map((i) => [i.label, i.kind, i.outcome, i.durationMs, i.subagentId])).toEqual([
      ["cargo build", "command", "ok", 4200, null],
      ["cargo test --lib", "test", "failed", 90_000, "a1b2"],
      ["ls", "command", "unobserved", null, null],
      ["sleep 600", "command", "unobserved", null, null],
    ]);
    expect(timeline.failed).toBe(1);
    expect(timeline.unobserved).toBe(2);
  });

  it("never reads a duration off a call whose result was not read", () => {
    const [item] = projectRunTimeline([command("x", 1, { duration_ms: 5 }, { result: "unobserved" })], GONE, NOW, 100).items;
    expect(item.durationMs).toBeNull();
  });

  it("places an edit at the time the transcript recorded, not the append time", () => {
    const edit = row("session.edit", { occurred_at: at(5), result: "ok" }, { object: "/repo/a.rs", ts_utc: "2026-09-09T00:00:00.000Z" });
    const [item] = projectRunTimeline([edit], GONE, NOW, 100).items;
    expect(item.kind).toBe("edit");
    expect(item.atMs).toBe(T(5));
  });

  it("collapses a row that appears twice", () => {
    const one = command("ls", 1, {}, { result: "ok" });
    expect(projectRunTimeline([one, one], GONE, NOW, 100).items).toHaveLength(1);
  });
});

describe("segments", () => {
  it("measures each state between the lifecycle rows that bound it", () => {
    const rows = [
      state("prompt_submitted", 0),
      state("permission_request", 60),
      state("tool_finished", 90),
      state("turn_finished", 150),
      state("session_ended", 400),
    ];
    const timeline = projectRunTimeline(rows, GONE, NOW, 100);
    expect(timeline.segments.map((s) => [s.state, s.durationMs, s.bound])).toEqual([
      ["working", 60_000, "observed"],
      ["waiting_you", 30_000, "observed"],
      ["working", 60_000, "observed"],
      ["idle", 250_000, "observed"],
      ["ended", null, "observed"],
    ]);
    expect(timeline.spentMs).toEqual({ working: 120_000, waiting_you: 30_000, idle: 250_000 });
    // A state with no known span is absent, not zero.
    expect(timeline.spentMs.unknown).toBeUndefined();
  });

  it("calls the time before the first row unknown, never idle", () => {
    const window: RunWindow = { live: false, startedAt: T(-120) / 1000, finishedAt: T(60) / 1000 };
    const timeline = projectRunTimeline([state("prompt_submitted", 0)], window, NOW, 100);
    expect(timeline.segments.map((s) => [s.state, s.durationMs, s.bound])).toEqual([
      ["unknown", 120_000, "observed"],
      ["working", 60_000, "finished"],
    ]);
    expect(timeline.spentMs.idle).toBeUndefined();
  });

  it("runs the last span of a live run to now, and says so", () => {
    const timeline = projectRunTimeline([state("prompt_submitted", 0)], LIVE, T(30), 100);
    expect(timeline.segments).toHaveLength(1);
    expect(timeline.segments[0]).toMatchObject({ state: "working", durationMs: 30_000, bound: "now" });
  });

  it("does not stretch the last span of a run that is gone and did not end in a row", () => {
    const timeline = projectRunTimeline([state("prompt_submitted", 0)], GONE, NOW, 100);
    expect(timeline.segments).toHaveLength(1);
    expect(timeline.segments[0]).toMatchObject({ state: "unknown", durationMs: null, bound: "unknown", cause: "prompt_submitted" });
    expect(timeline.spentMs).toEqual({});
  });

  it("draws nothing for a run that left no lifecycle rows, and says there were none", () => {
    const timeline = projectRunTimeline([command("ls", 1, {}, { result: "ok" })], LIVE, NOW, 100);
    expect(timeline.segments).toEqual([]);
    expect(timeline.lifecycleRows).toBe(0);
    expect(spentSummary(timeline.spentMs)).toBe("");
  });

  it("orders rows by their own time, not the order they were read in", () => {
    const rows = [state("turn_finished", 100), state("prompt_submitted", 0)];
    const timeline = projectRunTimeline(rows, GONE, NOW, 100);
    expect(timeline.segments.map((s) => [s.state, s.durationMs])).toEqual([
      ["working", 100_000],
      ["unknown", null],
    ]);
  });

  it("ignores a lifecycle row that names no known event or carries no time", () => {
    const rows = [
      row("session.state", { event: "something_new", occurred_at: at(1) }),
      row("session.state", { event: "prompt_submitted", occurred_at: "not a time" }, { ts_utc: "garbage" }),
    ];
    const timeline = projectRunTimeline(rows, LIVE, NOW, 100);
    expect(timeline.lifecycleRows).toBe(0);
  });

  it("splits working where a command measurably ran", () => {
    const rows = [
      state("prompt_submitted", 0),
      state("turn_finished", 100),
      command("cargo test", 20, { duration_ms: 30_000 }, { result: "ok" }),
      // Not measured: its time stays working.
      command("sleep 1", 70, {}, { result: "unobserved" }),
    ];
    const timeline = projectRunTimeline(rows, GONE, NOW, 100);
    expect(timeline.segments.map((s) => [s.state, s.durationMs])).toEqual([
      ["working", 20_000],
      ["waiting_tool", 30_000],
      ["working", 50_000],
      ["unknown", null],
    ]);
    expect(timeline.spentMs).toEqual({ working: 70_000, waiting_tool: 30_000 });
  });

  it("clips a command that outlasts the span it ran in", () => {
    const rows = [
      state("prompt_submitted", 0),
      state("permission_request", 50),
      command("cargo build", 40, { duration_ms: 60_000 }, { result: "ok" }),
    ];
    const timeline = projectRunTimeline(rows, GONE, NOW, 100);
    const [working, wait] = timeline.segments;
    expect(working).toMatchObject({ state: "working", durationMs: 40_000 });
    expect(wait).toMatchObject({ state: "waiting_tool", durationMs: 10_000 });
  });

  it("shares a bar only over the time it knows", () => {
    const rows = [state("prompt_submitted", 0), state("turn_finished", 75), state("prompt_submitted", 100)];
    const shares = segmentShares(projectRunTimeline(rows, GONE, NOW, 100).segments);
    expect(shares.map((s) => [s.state, Math.round(s.pct)])).toEqual([
      ["working", 75],
      ["idle", 25],
    ]);
    expect(segmentShares([])).toEqual([]);
  });
});

describe("tokens", () => {
  const usage = (input: number, output: number, cacheRead: number, cacheCreation: number, seconds = 1) =>
    row("session.usage", { occurred_at: at(seconds), usage: { input, output, cache_read: cacheRead, cache_creation: cacheCreation } });

  it("sums each response once and reports how many", () => {
    const timeline = projectRunTimeline([usage(3, 400, 1000, 70), usage(2, 100, 2000, 0, 9)], GONE, NOW, 100);
    expect(timeline.tokens).toEqual({ input: 5, output: 500, cacheRead: 3000, cacheCreation: 70, total: 3575, responses: 2 });
    expect(timeline.lastActivityMs).toBe(T(9));
  });

  it("reports no tokens, not zero tokens, when none were recorded", () => {
    expect(projectRunTimeline([command("ls", 1, {}, { result: "ok" })], GONE, NOW, 100).tokens).toBeNull();
  });

  it("does not sum a response whose counts are not all numbers", () => {
    const bad = row("session.usage", { usage: { input: 1, output: "many", cache_read: 0, cache_creation: 0 } });
    const negative = row("session.usage", { usage: { input: -1, output: 1, cache_read: 0, cache_creation: 0 } });
    expect(projectRunTimeline([bad, negative], GONE, NOW, 100).tokens).toBeNull();
  });

  it("formats counts for humans", () => {
    expect([842, 12_400, 3_100_000].map(formatTokens)).toEqual(["842", "12.4k", "3.1M"]);
    expect(formatTokens(Number.NaN)).toBe("—");
  });
});

describe("bounds", () => {
  it("says it was capped when the read filled its limit", () => {
    const rows = [command("a", 1, {}, { result: "ok" }), command("b", 2, {}, { result: "ok" })];
    expect(projectRunTimeline(rows, GONE, NOW, 2).capped).toBe(true);
    expect(projectRunTimeline(rows, GONE, NOW, 3).capped).toBe(false);
  });

  it("reports the latest time any row carries", () => {
    const rows = [command("a", 5, {}, { result: "ok" }), state("turn_finished", 50), command("b", 7, {}, { result: "ok" })];
    expect(projectRunTimeline(rows, GONE, NOW, 100).lastActivityMs).toBe(T(50));
    expect(projectRunTimeline([], GONE, NOW, 100).lastActivityMs).toBeNull();
  });
});
