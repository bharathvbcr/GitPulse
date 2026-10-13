import { describe, expect, it } from "vitest";
import {
  STATUS_POLL_COST_FACTOR,
  STATUS_POLL_INTERVAL_MS,
  STATUS_POLL_MAX_INTERVAL_MS,
  STATUS_POLL_QUIET_AFTER,
  STATUS_POLL_QUIET_INTERVAL_MS,
  statusPollRestMs,
} from "./statusPoll";

describe("statusPollRestMs", () => {
  it("keeps the usual interval for a cheap poll on a repository that is changing", () => {
    expect(statusPollRestMs({ watchLive: true, lastDurationMs: 40, quietPolls: 0 })).toBe(STATUS_POLL_INTERVAL_MS);
    expect(statusPollRestMs({ watchLive: false, lastDurationMs: null, quietPolls: 0 })).toBe(STATUS_POLL_INTERVAL_MS);
  });

  it("rests a multiple of what the last poll cost, so the duty cycle stays under 10%", () => {
    for (const cost of [1_000, 4_000, 9_400]) {
      const rest = statusPollRestMs({ watchLive: true, lastDurationMs: cost, quietPolls: 0 });
      expect(rest).toBeGreaterThanOrEqual(STATUS_POLL_COST_FACTOR * cost);
      expect(cost / (cost + rest)).toBeLessThanOrEqual(0.1);
    }
  });

  it("lengthens to the quiet interval only for a live watcher, and only after enough quiet polls", () => {
    const quiet = STATUS_POLL_QUIET_AFTER;
    expect(statusPollRestMs({ watchLive: true, lastDurationMs: 40, quietPolls: quiet - 1 })).toBe(STATUS_POLL_INTERVAL_MS);
    expect(statusPollRestMs({ watchLive: true, lastDurationMs: 40, quietPolls: quiet })).toBe(STATUS_POLL_QUIET_INTERVAL_MS);
    // Without a watcher the poll is the only thing keeping the view current.
    expect(statusPollRestMs({ watchLive: false, lastDurationMs: 40, quietPolls: quiet * 10 })).toBe(STATUS_POLL_INTERVAL_MS);
  });

  it("is bounded however slow the last poll was, and ignores a reading that is not a duration", () => {
    expect(statusPollRestMs({ watchLive: true, lastDurationMs: 10 * 60_000, quietPolls: 0 })).toBe(STATUS_POLL_MAX_INTERVAL_MS);
    for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, -5, 0]) {
      expect(statusPollRestMs({ watchLive: true, lastDurationMs: bad, quietPolls: 0 })).toBe(STATUS_POLL_INTERVAL_MS);
    }
  });
});
