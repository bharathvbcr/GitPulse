import { describe, expect, it } from "vitest";
import { describeDelta, deltaTone, formatDelta } from "./format";
import { deltaFrom, readCell, type CellDelta } from "./types";

const delta = (change: number, from = 100, day = "2026-09-01"): CellDelta => ({
  change,
  from,
  day,
});

describe("deltaFrom", () => {
  it("returns nothing at all when there is no baseline", () => {
    // The rule the whole history table exists to preserve: a first scan has no
    // direction, and a zero-valued delta would be a claim about a past nobody
    // observed.
    expect(deltaFrom(100, null, "2026-09-01")).toBeUndefined();
    expect(deltaFrom(100, 90, null)).toBeUndefined();
    expect(deltaFrom(100, undefined, undefined)).toBeUndefined();
    expect(deltaFrom(100, 90, "")).toBeUndefined();
  });

  it("refuses a non-finite reading rather than producing NaN", () => {
    expect(deltaFrom(Number.NaN, 90, "2026-09-01")).toBeUndefined();
    expect(deltaFrom(100, Number.POSITIVE_INFINITY, "2026-09-01")).toBeUndefined();
  });

  it("carries the baseline and its day, not just the change", () => {
    // Families are scanned independently, so a delta whose baseline is unstated
    // invites the reader to assume "since yesterday" — which is often months
    // wrong on the same row.
    expect(deltaFrom(150, 100, "2026-08-01")).toEqual({
      change: 50,
      from: 100,
      day: "2026-08-01",
    });
  });

  it("rides on the cell, so a reading and its direction cannot separate", () => {
    const cell = readCell(150, 1, false, deltaFrom(150, 100, "2026-08-01"));
    expect(cell.kind === "read" && cell.delta?.change).toBe(50);
  });
});

describe("formatDelta", () => {
  it("says nothing for an unchanged measurement", () => {
    // "+0" would make the chip appear for a repository where nothing happened,
    // which is precisely when it should be absent.
    expect(formatDelta(delta(0), "count")).toBe("");
    expect(formatDelta(delta(Number.NaN), "count")).toBe("");
  });

  it("spells each column in its own units", () => {
    expect(formatDelta(delta(1500), "count")).toBe("+1,500");
    expect(formatDelta(delta(1024 * 1024), "bytes")).toBe("+1.00 MB");
    expect(formatDelta(delta(-3.25), "percent")).toBe("−3.3pp");
  });

  it("uses a minus sign, not a hyphen, so columns of digits line up", () => {
    expect(formatDelta(delta(-40), "count")).toBe("−40");
  });
});

describe("deltaTone", () => {
  it("treats down as good only where down is good", () => {
    // Fewer vulnerabilities is an improvement; less coverage is not. Reading
    // the sign alone would colour a coverage drop green.
    expect(deltaTone(delta(-5), "lower")).toContain("emerald");
    expect(deltaTone(delta(-5), "higher")).toContain("amber");
    expect(deltaTone(delta(5), "higher")).toContain("emerald");
    expect(deltaTone(delta(5), "lower")).toContain("amber");
  });

  it("stays neutral where neither direction is a verdict", () => {
    // More lines of code is neither good nor bad.
    expect(deltaTone(delta(900), "neutral")).toBe("text-textMuted");
    expect(deltaTone(delta(0), "lower")).toBe("text-textMuted");
  });
});

describe("describeDelta", () => {
  it("always names the day it is measuring from", () => {
    const text = describeDelta(delta(50, 100, "2026-08-01"), "count", "Lines of code");
    expect(text).toContain("2026-08-01");
    expect(text).toContain("100");
    expect(text).toContain("up");
  });

  it("says unchanged rather than implying a move", () => {
    expect(describeDelta(delta(0, 100, "2026-08-01"), "count", "Vulnerabilities")).toContain(
      "unchanged since 2026-08-01",
    );
  });

  it("renders the baseline in the column's own units", () => {
    expect(describeDelta(delta(1024, 2048, "2026-08-01"), "bytes", "Storage")).toContain("2.00 KB");
    expect(describeDelta(delta(1, 71.5, "2026-08-01"), "percent", "Coverage")).toContain("71.5%");
  });
});
