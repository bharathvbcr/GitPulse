import { describe, expect, it } from "vitest";
import type { PreviousBoot, TracePeaks, TraceRecord } from "../stores/hostLoadStore";
import { previousBootLines, traceRow } from "./hostLoad";

const record = (over: Partial<TraceRecord> = {}): TraceRecord => ({
  wall_s: 1_760_000_000,
  mono_ms: 10_000,
  boot_id: 7,
  pressure: "warn",
  tier: "elevated",
  binding: "swap",
  admit: true,
  swap_used: 40 * 2 ** 30,
  compressed_bytes: 2 ** 30,
  load_1m: 120,
  free_disk: 90 * 2 ** 30,
  launched_sessions: 3,
  observed_sessions: 32,
  largest_foreign: 2 ** 30,
  launched_footprint: 2 ** 30,
  thermal: "fair",
  power: "ac",
  segment_fraction: 0.4,
  ...over,
});

const peaks = (over: Partial<TracePeaks> = {}): TracePeaks => ({
  records: 2,
  max_tier: "severe",
  refused: true,
  max_swap_used: 57 * 2 ** 30,
  max_compressed_bytes: 2 ** 30,
  max_load_1m: 120,
  min_free_disk: 50 * 2 ** 30,
  max_launched_footprint: 2 ** 30,
  max_largest_foreign: 2 ** 30,
  max_segment_fraction: 0.8,
  worst_thermal: "fair",
  max_pressure: "warn",
  ...over,
});

const previous = (over: Partial<PreviousBoot> = {}): PreviousBoot => ({
  ended_wall_s: 1_760_000_020,
  records: [record({ mono_ms: 0 }), record({ mono_ms: 10_000 })],
  peaks: peaks(),
  error: null,
  ...over,
});

describe("previousBootLines", () => {
  it("reports the worst of each reading", () => {
    const text = previousBootLines(previous()).join("\n");
    expect(text).toContain("launches were refused");
    expect(text).toContain("at most 120.0");
    expect(text).toContain("0.80 of the limit");
  });

  it("says unreadable for a reading that could not be taken, never zero", () => {
    const text = previousBootLines(previous({ peaks: peaks({ max_swap_used: null, max_load_1m: null, min_free_disk: null }) })).join("\n");
    expect(text).toContain("Swap used, at most unreadable");
    expect(text).toContain("1-minute load, at most unreadable");
    expect(text).toContain("Free disk, at least unreadable");
  });

  it("names a trace that could not be read instead of showing an empty one", () => {
    expect(previousBootLines(previous({ error: "short file", records: [], ended_wall_s: null }))).toEqual([
      "The last boot's trace could not be read: short file.",
    ]);
  });

  it("says an empty trace holds no readings", () => {
    expect(previousBootLines(previous({ records: [], ended_wall_s: null }))).toEqual(["The last boot's trace holds no readings."]);
  });
});

describe("traceRow", () => {
  it("names the tier and its binding and marks refusal", () => {
    const row = traceRow(record({ admit: false }));
    expect(row[2]).toContain("elevated");
    expect(row[2]).toContain("(refusing)");
    expect(row).toHaveLength(7);
  });

  it("shows unreadable values as unreadable", () => {
    const row = traceRow(record({ pressure: null, tier: null, binding: null, swap_used: null, load_1m: null, free_disk: null, launched_sessions: null }));
    expect(row[1]).toBe("unreadable");
    expect(row[2]).toBe("unknown");
    expect(row.slice(3)).toEqual(["unreadable", "unreadable", "unreadable", "unreadable"]);
  });
});
