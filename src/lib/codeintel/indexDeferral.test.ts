import { describe, expect, it } from "vitest";
import { classifyMapFailure, mapFailureMessage } from "../tools/externalTools";
import { buildBlockReason } from "./buildBlock";
import { deferredStripText, formatAge, isStatusDeferred, lastGoodText } from "./indexDeferral";
import type { DevmapCliStatus } from "./types";

/** What `cmd_devmap_status` answers when the spawn gate shed the probe, after a reading was taken. */
function deferredStatus(overrides: Partial<NonNullable<DevmapCliStatus["deferral"]>> = {}): DevmapCliStatus {
  return {
    available: false,
    reason: "devmap deferred under load after 0.000s: background work is shed while the git spawn rate budget is low",
    status: null,
    deferral: {
      last_good: {
        generation_id: 5950,
        age_ms: 4 * 60_000 + 500,
        // The reading before the deferral said the index was fresh; the backend
        // does not carry that forward, because nothing re-checked it.
        fresh: null,
        fresh_reason: "devmap status was deferred under load, so freshness was not re-checked",
      },
      ...overrides,
    },
  };
}

describe("a deferred devmap status", () => {
  it("is recognised by the typed field, not by the wording of its reason", () => {
    expect(isStatusDeferred(deferredStatus())).toBe(true);
    const sameWords: DevmapCliStatus = { available: false, reason: deferredStatus().reason, status: null };
    expect(isStatusDeferred(sameWords)).toBe(false);
    expect(isStatusDeferred({ available: true, status: { is_fresh: true } })).toBe(false);
    expect(isStatusDeferred(null)).toBe(false);
  });

  it("is not read as a missing tool: no Setup notice, and Build stays available", () => {
    const cli = deferredStatus();
    // The failure the code map showed on 2026-11-21: "The code map tool is not
    // installed" over a healthy index, because every unavailable status was no_cli.
    expect(classifyMapFailure({ cliAvailable: false, mapAvailable: true })).toBe("no_cli");
    expect(
      classifyMapFailure({ cliAvailable: cli.available, cliDeferred: isStatusDeferred(cli), mapAvailable: true }),
    ).toBe("other");
    expect(mapFailureMessage("other", undefined)).not.toContain("not installed");
    // The panel's `cliKnownAbsent` is false for it, so Build is not blocked as "tool missing".
    expect(buildBlockReason(false, "deferred")).toBeNull();
  });

  it("names the last good generation and its age, and never says the index is fresh", () => {
    const { headline, detail } = deferredStripText(deferredStatus().deferral!);
    expect(headline).toBe("Index check deferred under load");
    expect(detail).toContain("generation 5950");
    expect(detail).toContain("4 min ago");
    expect(detail).toContain("not re-checked");
    for (const text of [headline, detail]) expect(text).not.toMatch(/\bfresh\b/i);
  });

  it("says a reading that was already stale was stale, and a missing reading is not a number", () => {
    const stale = lastGoodText({ generation_id: 7, age_ms: 1_000, fresh: false, fresh_reason: "" });
    expect(stale).toContain("already stale");
    const none = deferredStripText({ last_good: null });
    expect(none.detail).toContain("No earlier reading");
    expect(none.detail).not.toMatch(/generation/);
    expect(lastGoodText({ generation_id: null, age_ms: 0, fresh: null, fresh_reason: "" })).toContain("generation unknown");
  });

  it("formats an age without rounding up", () => {
    expect(formatAge(0)).toBe("0 s");
    expect(formatAge(59_999)).toBe("59 s");
    expect(formatAge(60_000)).toBe("1 min");
    expect(formatAge(3 * 3_600_000 + 1)).toBe("3 h");
    expect(formatAge(72 * 3_600_000)).toBe("3 d");
    expect(formatAge(Number.NaN)).toBe("0 s");
    expect(formatAge(-5)).toBe("0 s");
  });
});
