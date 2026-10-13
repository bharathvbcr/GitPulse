import { describe, expect, it } from "vitest";
import {
  STATUS_POLL_COST_FACTOR,
  STATUS_POLL_INTERVAL_MS,
  STATUS_POLL_MAX_INTERVAL_MS,
  statusPollRestMs,
} from "./statusPoll";

describe("statusPollRestMs", () => {
  it("keeps the usual interval for a cheap poll, however long nothing has changed", () => {
    // There is no input for "quiet": the worktree watch is non-recursive
    // (watcher/debouncer.rs), so the poll is the primary detector of nested
    // edits and a quiet stretch must not slow it.
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
