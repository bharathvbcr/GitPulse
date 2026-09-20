import { describe, expect, it } from "vitest";
import type { IntervalHost } from "../dom/visibleInterval";
import { runPhase, runTimelineRow } from "../github/runLifecycle";
import type { WorkflowRunInfo } from "../github/types";
import { createLivePoll } from "./livePoll";
import { isSettled, isVerdict, type MonitorPhase } from "./phase";
import {
  afterPoll,
  backoffDelayMs,
  decidePoll,
  resolveLimits,
  type PollLimits,
  type PollState,
} from "./pollSchedule";
import {
  barWidthPct,
  displayText,
  durationMs,
  formatDuration,
  longestDurationMs,
  GLANCE_NAME_CHARS,
  glanceName,
  glanceState,
  MAX_LABEL_CHARS,
  medianSettledDurationMs,
  parseInstant,
  TIMELINE_PREVIEW_COUNT,
  type TimelineRow,
} from "./timeline";
import { anyInFlight, failuresSince, settledSince, verdictRate } from "./transitions";
import { overflowsPreview, previewSlice } from "../ui/previewList";

/**
 * Seeded so a failure is reproducible.
 *
 * A fuzz test that picks fresh randomness each run reports a different defect
 * every time and none of them twice, which is indistinguishable from flakiness
 * and gets muted rather than fixed.
 */
function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const pick = <T>(random: () => number, values: readonly T[]): T =>
  values[Math.floor(random() * values.length) % values.length];

/** Every word gh can put in either field, plus shapes it never would. */
const STATUS_WORDS = [
  "queued",
  "completed",
  "in_progress",
  "requested",
  "waiting",
  "pending",
  "",
  "COMPLETED",
  " completed ",
  "teleported",
  "null",
  "undefined",
  "0",
  "in progress",
];
const CONCLUSION_WORDS = [
  "action_required",
  "cancelled",
  "failure",
  "neutral",
  "skipped",
  "stale",
  "startup_failure",
  "success",
  "timed_out",
  "",
  "SUCCESS",
  " success ",
  "successish",
  "fail",
];

const INSTANTS = [
  "2026-09-17T08:00:00Z",
  "2026-09-17T08:05:00Z",
  "2026-09-17T07:00:00Z",
  "1970-01-01T00:00:00Z",
  "9999-12-31T23:59:59Z",
  "",
  "   ",
  "not-a-date",
  "2026-13-45T99:99:99Z",
  "0",
];

function fuzzRun(random: () => number, id: number): WorkflowRunInfo {
  return {
    id,
    name: pick(random, ["ci", "release", "", "a".repeat(500)]),
    title: pick(random, ["build", "", "x".repeat(1000), "multi\nline\ttitle"]),
    status: pick(random, STATUS_WORDS),
    conclusion: pick(random, CONCLUSION_WORDS),
    head_branch: pick(random, ["main", "", "feat/x"]),
    url: pick(random, ["https://example.invalid/1", ""]),
    created_at: pick(random, INSTANTS),
    started_at: pick(random, INSTANTS),
    updated_at: pick(random, INSTANTS),
    head_sha: pick(random, ["0123456789abcdef0123456789abcdef01234567", "", "zz"]),
    event: pick(random, ["push", "pull_request", "workflow_dispatch", ""]),
  };
}

describe("the run vocabulary under fuzz", () => {
  it("never crashes and never invents a verdict", () => {
    const random = rng(0x5eed);
    const seen = new Set<MonitorPhase>();
    for (let i = 0; i < 20_000; i += 1) {
      const run = fuzzRun(random, i);
      const phase = runPhase(run);
      seen.add(phase);
      expect(["in_flight", "settled_ok", "settled_bad", "unknown"]).toContain(phase);

      const status = run.status.trim().toLowerCase();
      const conclusion = run.conclusion.trim().toLowerCase();
      if (phase === "settled_ok") {
        // The only route to a green verdict, exhaustively.
        expect(status, JSON.stringify(run)).toBe("completed");
        expect(conclusion).toBe("success");
      }
      if (phase === "settled_bad") {
        expect(status, JSON.stringify(run)).toBe("completed");
        expect(["failure", "cancelled", "timed_out", "startup_failure"]).toContain(conclusion);
      }
      if (phase === "in_flight") {
        expect(
          ["queued", "in_progress", "requested", "waiting", "pending"],
          JSON.stringify(run),
        ).toContain(status);
      }
    }
    // Non-vacuity: a fuzz run that only ever produced `unknown` would pass
    // every assertion above while testing nothing.
    for (const phase of ["in_flight", "settled_ok", "settled_bad", "unknown"] as const) {
      expect(seen.has(phase), `fuzz never produced ${phase}`).toBe(true);
    }
  });

  it("produces a row whose every field is a bounded single line", () => {
    const random = rng(0xc0ffee);
    for (let i = 0; i < 5_000; i += 1) {
      const row = runTimelineRow(fuzzRun(random, i));
      for (const [key, value] of Object.entries(row)) {
        expect(typeof value, key).toBe("string");
        // No line terminator may reach a row: one newline in a label pushes
        // every later row down the page.
        expect(/[\n\r\u2028\u2029]/.test(value as string), `${key}: ${value}`).toBe(
          false,
        );
      }
      expect(row.label.length, "a row must always have a label").toBeGreaterThan(0);
      expect([...row.label].length).toBeLessThanOrEqual(MAX_LABEL_CHARS + 1);
      expect([...row.sublabel].length).toBeLessThanOrEqual(MAX_LABEL_CHARS + 1);
      const name = glanceName(row);
      expect(/[\n\r\u2028\u2029]/.test(name)).toBe(false);
      expect([...name].length).toBeLessThanOrEqual(GLANCE_NAME_CHARS + 1);
      expect(["Pass", "Fail", "Live", "—"]).toContain(glanceState(row.phase));
    }
  });
});

describe("display text under hostile input", () => {
  it("bounds length without splitting a code point", () => {
    // Family emoji are multi-code-point; a naive `slice` cuts mid-surrogate
    // and renders a replacement character.
    const astral = "👩‍👩‍👧‍👦".repeat(400);
    const out = displayText(astral);
    expect([...out].length).toBeLessThanOrEqual(MAX_LABEL_CHARS + 1);
    expect(out).not.toContain("�");
    for (const ch of out) {
      const code = ch.codePointAt(0) ?? 0;
      expect(code >= 0xd800 && code <= 0xdfff, "no lone surrogate may survive").toBe(false);
    }
  });

  it("flattens every line terminator, including the exotic ones", () => {
    const out = displayText("a\nb\r\nc\u2028d\u2029e");
    expect(/[\n\r\u2028\u2029]/.test(out)).toBe(false);
    expect(out).toContain("a");
    expect(out).toContain("e");
  });

  it("returns an empty string for anything that is not one", () => {
    for (const value of [null, undefined, 0, 1, {}, [], Number.NaN, () => {}]) {
      expect(displayText(value)).toBe("");
    }
  });
});

describe("timeline arithmetic at scale and under hostile clocks", () => {
  function fuzzRows(random: () => number, count: number): TimelineRow[] {
    return Array.from({ length: count }, (_, i) => runTimelineRow(fuzzRun(random, i)));
  }

  it("keeps every derived number finite over a large sample", () => {
    const random = rng(0xabcdef);
    const rows = fuzzRows(random, 10_000);
    const clocks = [
      0,
      Date.parse("2026-09-17T08:02:30Z"),
      Date.parse("1990-01-01T00:00:00Z"),
      Number.MAX_SAFE_INTEGER,
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
    ];
    for (const now of clocks) {
      const longest = longestDurationMs(rows, now);
      expect(longest === null || Number.isFinite(longest), `longest at now=${now}`).toBe(true);
      if (longest !== null) expect(longest).toBeGreaterThanOrEqual(0);

      for (const row of rows) {
        const ms = durationMs(row, now);
        // Null or a non-negative finite number. Never NaN, never negative:
        // both render as a plausible-looking bar.
        expect(ms === null || (Number.isFinite(ms) && ms >= 0), `duration ${ms}`).toBe(true);

        const width = barWidthPct(ms, longest);
        expect(width === null || (width >= 0 && width <= 100), `width ${width}`).toBe(true);

        const text = formatDuration(ms);
        expect(text).not.toContain("NaN");
        expect(text).not.toContain("Infinity");
        expect(text).not.toContain("-");
      }

      const median = medianSettledDurationMs(rows, now);
      expect(median.sample).toBeGreaterThanOrEqual(0);
      expect(
        median.medianMs === null || (Number.isFinite(median.medianMs) && median.medianMs >= 0),
      ).toBe(true);
      // A figure must never be reported without the sample behind it.
      expect(median.medianMs === null).toBe(median.sample === 0);
    }
  });

  it("never lets a clock going backwards produce a measurement", () => {
    const random = rng(7);
    const rows = fuzzRows(random, 2_000);
    let sawInFlight = false;
    for (const row of rows) {
      if (row.phase !== "in_flight") continue;
      if (parseInstant(row.startedAt) === null) continue;
      sawInFlight = true;
      const started = parseInstant(row.startedAt)!;
      expect(durationMs(row, started - 1), "a clock before the start measures nothing").toBeNull();
      expect(durationMs(row, started)).toBe(0);
    }
    expect(sawInFlight, "non-vacuity: the fuzz must have produced in-flight rows").toBe(true);
  });
});

describe("transitions under fuzz", () => {
  const phases: MonitorPhase[] = ["in_flight", "settled_ok", "settled_bad", "unknown"];

  it("never reports a transition it cannot have observed", () => {
    const random = rng(0x1234);
    let sawTransition = false;
    for (let iteration = 0; iteration < 5_000; iteration += 1) {
      const ids = Array.from({ length: 1 + Math.floor(random() * 8) }, (_, i) => `r${i}`);
      const previous = ids
        .filter(() => random() > 0.25)
        .map((id) => ({ id, phase: pick(random, phases) }));
      const next = ids
        .filter(() => random() > 0.25)
        .map((id) => ({ id, phase: pick(random, phases) }));

      const transitions = settledSince(previous, next);
      if (transitions.length > 0) sawTransition = true;

      const prevById = new Map(previous.map((r) => [r.id, r]));
      const nextById = new Map(next.map((r) => [r.id, r]));
      for (const t of transitions) {
        // Present on both sides, moving to settled, from in flight. Nothing
        // else may ever be announced.
        expect(prevById.has(t.row.id)).toBe(true);
        expect(nextById.has(t.row.id)).toBe(true);
        expect(t.from).toBe("in_flight");
        expect(isSettled(t.to)).toBe(true);
      }
      // A baseline observation announces nothing, whatever it contains.
      expect(settledSince([], next)).toEqual([]);
      // Failures are a subset, and only ever the bad verdict.
      for (const t of failuresSince(previous, next)) expect(t.to).toBe("settled_bad");

      const rate = verdictRate(next);
      expect(rate.judged + rate.unjudged).toBe(next.length);
      expect(rate.passed + rate.failed).toBe(rate.judged);
      expect(rate.ratePct === null).toBe(rate.judged === 0);
      if (rate.ratePct !== null) {
        expect(rate.ratePct).toBeGreaterThanOrEqual(0);
        expect(rate.ratePct).toBeLessThanOrEqual(100);
      }
      expect(anyInFlight(next)).toBe(next.some((r) => r.phase === "in_flight"));
    }
    expect(sawTransition, "non-vacuity: the fuzz must have produced transitions").toBe(true);
  });

  it("is idempotent: re-diffing the same pair changes nothing", () => {
    const random = rng(99);
    for (let i = 0; i < 1_000; i += 1) {
      const previous = [{ id: "a", phase: pick(random, phases) }];
      const next = [{ id: "a", phase: pick(random, phases) }];
      const first = settledSince(previous, next);
      expect(settledSince(previous, next)).toEqual(first);
      // And settled-to-settled never fires, so nothing is announced twice.
      expect(settledSince(next, next)).toEqual([]);
    }
  });
});

describe("the scheduler under fuzz", () => {
  it("always returns a bounded, finite decision", () => {
    const random = rng(0xfeed);
    const kinds = new Set<string>();
    for (let i = 0; i < 20_000; i += 1) {
      const limits: PollLimits = {
        baseIntervalMs: pick(random, [1, 100, 6_000, 0, -5, Number.NaN, 1e12]),
        maxIntervalMs: pick(random, [1, 60_000, 0, -1, Number.NaN, 1e12]),
        maxDurationMs: pick(random, [1, 900_000, 0, Number.NaN, Number.POSITIVE_INFINITY]),
        maxPolls: pick(random, [1, 200, 0, -3, Number.NaN]),
        maxFailures: pick(random, [1, 5, 0, Number.NaN]),
      };
      const state: PollState = {
        startedAt: pick(random, [null, 0, 1_000, Number.MAX_SAFE_INTEGER, -1_000]),
        polls: pick(random, [0, 1, 199, 1e9, Number.MAX_SAFE_INTEGER]),
        failures: pick(random, [0, 1, 4, 1e9, Number.MAX_SAFE_INTEGER]),
      };
      const now = pick(random, [0, 1_000, 1e12, Number.NaN, -1e9]);
      const anyMoving = random() > 0.2;

      const decision = decidePoll({ anyInFlight: anyMoving, state, now, limits });
      kinds.add(decision.kind);
      expect(["poll", "idle", "exhausted"]).toContain(decision.kind);
      expect(Number.isFinite(decision.delayMs), JSON.stringify({ limits, state, now })).toBe(
        true,
      );
      expect(decision.delayMs).toBeGreaterThanOrEqual(0);
      expect(decision.reason.trim()).not.toBe("");

      const resolved = resolveLimits(limits);
      if (decision.kind === "poll") {
        // A delay must be usable as an interval: at least a millisecond, never
        // beyond the resolved ceiling.
        expect(decision.delayMs).toBeGreaterThanOrEqual(1);
        expect(decision.delayMs).toBeLessThanOrEqual(resolved.maxIntervalMs);
      } else {
        expect(decision.delayMs).toBe(0);
      }
      if (!anyMoving) expect(decision.kind).toBe("idle");

      // The backoff is monotonic in failures and never exceeds the ceiling.
      const a = backoffDelayMs(2, limits);
      const b = backoffDelayMs(3, limits);
      expect(b).toBeGreaterThanOrEqual(a);
      expect(b).toBeLessThanOrEqual(resolved.maxIntervalMs);

      // Accounting stays inside safe integers however absurd the input.
      const advanced = afterPoll(state, { ok: random() > 0.5 }, now);
      expect(Number.isSafeInteger(advanced.polls)).toBe(true);
      expect(Number.isSafeInteger(advanced.failures)).toBe(true);
      expect(advanced.polls).toBeGreaterThanOrEqual(0);
    }
    for (const kind of ["poll", "idle", "exhausted"]) {
      expect(kinds.has(kind), `fuzz never produced ${kind}`).toBe(true);
    }
  });
});

describe("the live loop under sustained churn", () => {
  /**
   * Drives the loop through thousands of ticks while flipping visibility,
   * failing polls and toggling work at random, then checks the invariants that
   * must survive all of it.
   */
  it("never overlaps, never leaks a timer, and never outspends its budget", async () => {
    const random = rng(0xd15ea5e);
    const limits: PollLimits = {
      baseIntervalMs: 1_000,
      maxIntervalMs: 4_000,
      maxDurationMs: 100_000,
      maxPolls: 8,
      maxFailures: 4,
    };

    const timers = new Map<number, () => void>();
    const listeners = new Set<() => void>();
    let nextHandle = 1;
    let clock = 0;
    let hidden = false;
    const host: IntervalHost = {
      setInterval: (handler) => {
        const handle = nextHandle++;
        timers.set(handle, handler);
        return handle;
      },
      clearInterval: (handle) => void timers.delete(handle as number),
      addEventListener: (_t, listener) => listeners.add(listener),
      removeEventListener: (_t, listener) => listeners.delete(listener),
      isHidden: () => hidden,
    };

    let concurrent = 0;
    let maxConcurrent = 0;
    let pollsThisSession = 0;
    let worstSessionPolls = 0;
    let lastState = "idle";
    const live = createLivePoll({
      poll: async () => {
        concurrent += 1;
        maxConcurrent = Math.max(maxConcurrent, concurrent);
        pollsThisSession += 1;
        worstSessionPolls = Math.max(worstSessionPolls, pollsThisSession);
        await Promise.resolve();
        concurrent -= 1;
        return random() > 0.4;
      },
      onState: (s) => (lastState = s.kind),
      limits,
      host,
      now: () => clock,
    });

    let moving = true;
    live.sync(true);
    for (let i = 0; i < 4_000; i += 1) {
      clock += 1 + Math.floor(random() * 5_000);
      if (random() < 0.02) {
        hidden = !hidden;
        for (const listener of [...listeners]) listener();
      }
      if (random() < 0.01) {
        moving = !moving;
        if (!moving) pollsThisSession = 0;
        live.sync(moving);
      }
      if (random() < 0.005) {
        live.reset();
        pollsThisSession = 0;
      }
      for (const handler of [...timers.values()]) handler();
      // Two turns: the poll awaits once internally before settling.
      await Promise.resolve();
      await Promise.resolve();

      // Timer invariants, checked every iteration rather than at the end.
      if (lastState === "idle" || lastState === "paused") {
        expect(timers.size, `a ${lastState} loop must hold no timer`).toBe(0);
      }
      expect(timers.size, "at most one timer ever").toBeLessThanOrEqual(1);
      expect(listeners.size, "one visibility listener per live timer").toBeLessThanOrEqual(1);
    }

    expect(maxConcurrent, "two polls must never be in flight at once").toBe(1);
    expect(
      worstSessionPolls,
      "no session may exceed its poll budget however the sampling churns",
    ).toBeLessThanOrEqual(limits.maxPolls);

    live.dispose();
    expect(timers.size).toBe(0);
    expect(listeners.size).toBe(0);
  });

  it("spends nothing at all over a long idle stretch", async () => {
    // The common case, and the one a timer leak would show up in as a slow
    // battery drain nobody attributes to this panel.
    const timers = new Map<number, () => void>();
    let nextHandle = 1;
    const host: IntervalHost = {
      setInterval: (handler) => {
        const handle = nextHandle++;
        timers.set(handle, handler);
        return handle;
      },
      clearInterval: (handle) => void timers.delete(handle as number),
      addEventListener: () => {},
      removeEventListener: () => {},
      isHidden: () => false,
    };
    let polls = 0;
    const live = createLivePoll({
      poll: () => {
        polls += 1;
        return Promise.resolve(true);
      },
      onState: () => {},
      host,
      now: () => 0,
    });
    for (let i = 0; i < 10_000; i += 1) {
      live.sync(false);
      for (const handler of [...timers.values()]) handler();
      await Promise.resolve();
    }
    expect(polls).toBe(0);
    expect(timers.size).toBe(0);
    live.dispose();
  });
});

describe("state machine exhaustiveness", () => {
  it("classifies every phase as exactly one of settled or in flight", () => {
    for (const phase of ["in_flight", "settled_ok", "settled_bad", "unknown"] as const) {
      expect(isSettled(phase)).toBe(phase !== "in_flight");
      expect(isVerdict(phase)).toBe(phase === "settled_ok" || phase === "settled_bad");
      // A verdict is always settled. The reverse does not hold, which is the
      // whole reason `unknown` exists.
      if (isVerdict(phase)) expect(isSettled(phase)).toBe(true);
    }
  });

  it("keeps an idle session from ever being reported as exhausted", () => {
    // Nothing in flight outranks every budget: a repository with no delivery
    // activity must never show "live updates paused".
    const spent: PollState = { startedAt: 0, polls: 1e9, failures: 1e9 };
    expect(decidePoll({ anyInFlight: false, state: spent, now: 1e12 }).kind).toBe("idle");
  });
});

describe("preview collapse under fuzz", () => {
  it("never hides a non-empty listing, and agrees with the expander", () => {
    const random = rng(0x51ce);
    const counts = [
      TIMELINE_PREVIEW_COUNT,
      0,
      -1,
      1,
      3,
      5,
      8,
      20,
      Number.NaN,
      Number.POSITIVE_INFINITY,
      Number.NEGATIVE_INFINITY,
      1.9,
      1e12,
    ];
    let sawHidden = false;
    for (let i = 0; i < 5_000; i += 1) {
      const n = Math.floor(random() * 40);
      const count = pick(random, counts);
      const expanded = random() > 0.5;
      const shown = previewSlice(Array.from({ length: n }), expanded, count);
      if (n > 0) expect(shown.length).toBeGreaterThan(0);
      if (n === 0) expect(shown).toEqual([]);
      if (expanded) expect(shown).toHaveLength(n);
      else expect(shown.length).toBeLessThanOrEqual(Math.max(n, 0));
      const hidden = n - previewSlice(Array.from({ length: n }), false, count).length;
      expect(overflowsPreview(n, count)).toBe(hidden > 0);
      if (hidden > 0) sawHidden = true;
    }
    expect(sawHidden, "non-vacuity: the fuzz must have hidden rows").toBe(true);
  });
});
