import { describe, expect, it } from "vitest";
import { formatDate, formatRelativeTime } from "../format";
import {
  TIMESTAMP_STYLES,
  formatAbsoluteDate,
  formatTimestamp,
  isTimestampStyle,
  timestampTitle,
} from "./timestampStyle";

/** 2026-09-06 12:00:00 local, and a "now" three days later. */
const SAMPLE = Math.floor(new Date(2026, 8, 6, 12, 0, 0).getTime() / 1000);
const LATER = SAMPLE + 3 * 86_400;

describe("timestamp style", () => {
  it("accepts exactly the two styles", () => {
    for (const style of TIMESTAMP_STYLES) expect(isTimestampStyle(style)).toBe(true);
    for (const value of ["iso", "", null, undefined, 0]) {
      expect(isTimestampStyle(value)).toBe(false);
    }
  });
});

describe("formatAbsoluteDate", () => {
  it("renders local calendar date, zero-padded and sortable", () => {
    expect(formatAbsoluteDate(SAMPLE)).toBe("2026-09-06");
  });

  it("stays fixed width across the year, which is why the column can be sized", () => {
    const december = Math.floor(new Date(2026, 11, 25, 9, 30).getTime() / 1000);
    expect(formatAbsoluteDate(december)).toBe("2026-12-25");
    expect(formatAbsoluteDate(december)).toHaveLength(
      formatAbsoluteDate(SAMPLE).length,
    );
  });

  it("returns empty for a falsy or unusable timestamp, like formatRelativeTime", () => {
    // Call sites lean on `|| fallback`; a style flip must not turn that into
    // the epoch or "NaN-NaN-NaN".
    expect(formatAbsoluteDate(0)).toBe("");
    expect(formatAbsoluteDate(Number.NaN)).toBe("");
  });
});

describe("formatTimestamp", () => {
  it("routes each style to its formatter", () => {
    expect(formatTimestamp(SAMPLE, "relative", LATER)).toBe(
      formatRelativeTime(SAMPLE, LATER),
    );
    expect(formatTimestamp(SAMPLE, "relative", LATER)).toBe("3d ago");
    expect(formatTimestamp(SAMPLE, "absolute", LATER)).toBe("2026-09-06");
  });

  it("returns empty for a falsy timestamp in both styles", () => {
    expect(formatTimestamp(0, "relative")).toBe("");
    expect(formatTimestamp(0, "absolute")).toBe("");
  });
});

describe("timestampTitle", () => {
  it("always carries the form the label does not, so neither hides the other", () => {
    expect(timestampTitle(SAMPLE, "absolute", LATER)).toBe("3d ago");
    expect(timestampTitle(SAMPLE, "relative", LATER)).toBe(formatDate(SAMPLE));
  });

  it.each(TIMESTAMP_STYLES)("never repeats the visible text in %s", (style) => {
    expect(timestampTitle(SAMPLE, style, LATER)).not.toBe(
      formatTimestamp(SAMPLE, style, LATER),
    );
  });

  it("returns empty for a falsy timestamp so no tooltip is offered", () => {
    expect(timestampTitle(0, "relative")).toBe("");
    expect(timestampTitle(0, "absolute")).toBe("");
  });
});
