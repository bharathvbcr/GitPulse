import { describe, expect, it } from "vitest";
import {
  STATUS_POLL_COST_FACTOR,
  STATUS_POLL_INTERVAL_MS,
  STATUS_POLL_MAX_INTERVAL_MS,
  statusPollRequest,
  statusPollRestMs,
} from "./statusPoll";

describe("statusPollRequest", () => {
  it("is the only status read that lets the backend answer from a kept read", () => {
    expect(statusPollRequest("/repo")).toEqual({ repoPath: "/repo", skipIfUnchanged: true });
  });
});

describe("statusPollRestMs", () => {
  it("keeps the usual interval for a cheap poll, however long nothing has changed", () => {
    // There is no input for "quiet": the worktree watch is non-recursive on
    // Linux and Windows (watcher/debouncer.rs), so the poll is the primary
    // detector of nested edits there and a quiet stretch must not slow it. On
    // macOS the cadence is the same; the backend answers a poll that finds
    // nothing moved without spawning git (statusPollRequest).
    expect(statusPollRestMs({ lastDurationMs: 40 })).toBe(STATUS_POLL_INTERVAL_MS);
    expect(statusPollRestMs({ lastDurationMs: null })).toBe(STATUS_POLL_INTERVAL_MS);
  });

  it("rests a multiple of what the last poll cost, so the duty cycle stays under 10%", () => {
    for (const cost of [1_000, 4_000, 9_400]) {
      const rest = statusPollRestMs({ lastDurationMs: cost });
      expect(rest).toBeGreaterThanOrEqual(STATUS_POLL_COST_FACTOR * cost);
      expect(cost / (cost + rest)).toBeLessThanOrEqual(0.1);
    }
  });

  it("is bounded however slow the last poll was, and ignores a reading that is not a duration", () => {
    expect(statusPollRestMs({ lastDurationMs: 10 * 60_000 })).toBe(STATUS_POLL_MAX_INTERVAL_MS);
    for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, -5, 0]) {
      expect(statusPollRestMs({ lastDurationMs: bad })).toBe(STATUS_POLL_INTERVAL_MS);
    }
  });
});
