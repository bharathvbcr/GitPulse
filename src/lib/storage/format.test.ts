import { describe, expect, it } from "vitest";
import {
  deltaClass,
  formatAge,
  formatDelta,
  formatSnapshotTime,
  humanBytes,
  pctOf,
} from "./format";

describe("humanBytes", () => {
  it("renders exact small values", () => {
    expect(humanBytes(0)).toBe("0 B");
    expect(humanBytes(1)).toBe("1 B");
    expect(humanBytes(1023)).toBe("1023 B");
  });

  it("scales through the unit ladder", () => {
    expect(humanBytes(1024)).toBe("1.00 KB");
    expect(humanBytes(1536)).toBe("1.50 KB");
    expect(humanBytes(1024 * 1024)).toBe("1.00 MB");
    expect(humanBytes(3.5 * 1024 * 1024 * 1024)).toBe("3.50 GB");
    expect(humanBytes(2 ** 40)).toBe("1.00 TB");
  });

  it("drops decimals once large enough to read", () => {
    expect(humanBytes(150 * 1024 * 1024)).toBe("150 MB");
  });

  it("degrades invalid input to an em dash", () => {
    expect(humanBytes(-5)).toBe("—");
    expect(humanBytes(Number.NaN)).toBe("—");
    expect(humanBytes(Number.POSITIVE_INFINITY)).toBe("—");
  });
});

describe("pctOf", () => {
  it("computes bounded percentages", () => {
    expect(pctOf(25, 100)).toBeCloseTo(25);
    expect(pctOf(200, 100)).toBe(100);
    expect(pctOf(-10, 100)).toBe(0);
  });

  it("survives zero and non-finite totals", () => {
    expect(pctOf(10, 0)).toBe(0);
    expect(pctOf(10, Number.NaN)).toBe(0);
    expect(pctOf(Number.NaN, 100)).toBe(0);
  });
});

describe("formatDelta", () => {
  it("signs growth and shrinkage", () => {
    expect(formatDelta(1536)).toBe("+1.50 KB");
    expect(formatDelta(-320)).toBe("−320 B");
    expect(formatDelta(0)).toBe("no change");
    expect(formatDelta(Number.NaN)).toBe("no change");
  });
});

describe("deltaClass", () => {
  it("colors growth as warning and shrink as good", () => {
    expect(deltaClass(10)).toContain("amber");
    expect(deltaClass(-10)).toContain("emerald");
    expect(deltaClass(0)).toContain("textMuted");
  });
});

describe("formatSnapshotTime / formatAge", () => {
  const now = Date.UTC(2026, 7, 25, 12, 0, 0);

  it("renders relative ages coarsely", () => {
    expect(formatAge(now - 10_000, now)).toBe("just now");
    expect(formatAge(now - 60_000, now)).toBe("1m ago");
    expect(formatAge(now - 45 * 60_000, now)).toBe("45m ago");
    expect(formatAge(now - 5 * 3_600_000, now)).toBe("5h ago");
    expect(formatAge(now - 3 * 86_400_000, now)).toBe("3d ago");
  });

  it("falls back to an absolute stamp for old entries", () => {
    const stamp = formatAge(now - 60 * 86_400_000, now);
    expect(stamp).not.toBe("—");
    expect(stamp).not.toMatch(/ago$/);
  });

  it("guards garbage input", () => {
    expect(formatSnapshotTime(Number.NaN)).toBe("—");
    expect(formatAge(Number.NaN, now)).toBe("—");
    expect(formatSnapshotTime(0)).toBe("—");
  });

  it("produces a deterministic absolute string", () => {
    const ms = Date.UTC(2026, 7, 25, 9, 5, 0);
    expect(formatSnapshotTime(ms)).toBe(formatSnapshotTime(ms));
  });
});
