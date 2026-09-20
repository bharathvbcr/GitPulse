import { describe, expect, it } from "vitest";
import { formatSectionCount, isBounded, type SectionCount } from "./counts";

/**
 * This replaces a regex that read the panel's `<h3>` text and asserted that a
 * heading mentioning `.length` also mentioned a total or a cap word.
 *
 * That guard checked the shape of a sentence. A new section only had to look
 * compliant to pass it, and it could say nothing at all about whether the
 * number printed was the right one. Here the arithmetic has a single owner and
 * the property is proved over its whole input space: there is no
 * `SectionCount` for which this function prints the surviving row count alone.
 */
describe("formatSectionCount", () => {
  it("prints a bare number only when nothing was cut", () => {
    expect(formatSectionCount({ shown: 3, total: 3 })).toBe("3");
    expect(formatSectionCount({ shown: 0, total: 0 })).toBe("0");
  });

  it("names the observed total and what survived when a cap fired", () => {
    expect(formatSectionCount({ shown: 3, total: 12 })).toBe("12; showing 3");
  });

  it("marks a total that is itself a floor", () => {
    expect(formatSectionCount({ shown: 3, total: 12, atLeast: true })).toBe(
      "at least 12; showing 3",
    );
    expect(formatSectionCount({ shown: 3, total: 3, atLeast: true })).toBe("at least 3");
  });

  it("qualifies a filtered view rather than presenting it as a total", () => {
    // "Direct" filters the rows that survived the scan cap, so it has no total
    // of its own. An unqualified "3 direct" was a floor printed as a count.
    expect(
      formatSectionCount({ shown: 3, total: 3, atLeast: true, qualifier: "direct" }),
    ).toBe("at least 3 direct");
    expect(formatSectionCount({ shown: 3, total: 3, qualifier: "direct" })).toBe(
      "3 direct",
    );
  });

  it("never claims fewer items than the table beneath it lists", () => {
    // A backend that reports only what it returned must not be able to make a
    // heading contradict its own rows.
    expect(formatSectionCount({ shown: 9, total: 0 })).toBe("9");
    expect(formatSectionCount({ shown: 9, total: -4 })).toBe("9");
  });

  it("survives non-integer and negative inputs without inventing a count", () => {
    expect(formatSectionCount({ shown: 2.7, total: 9.9 })).toBe("9; showing 2");
    expect(formatSectionCount({ shown: -3, total: -1 })).toBe("0");
  });

  /**
   * The class, stated as a property rather than as a list of today's sections:
   * whenever the rendered rows are fewer than what the scan observed, the text
   * must say so. Exhaustive over a grid that covers every ordering of the two
   * numbers, both flags, and the qualifier.
   */
  it("discloses the shortfall for every count where one exists", () => {
    for (let shown = 0; shown <= 6; shown++) {
      for (let total = 0; total <= 6; total++) {
        for (const atLeast of [false, true]) {
          for (const qualifier of [undefined, "direct"]) {
            const count: SectionCount = { shown, total, atLeast, qualifier };
            const text = formatSectionCount(count);
            const observed = Math.max(shown, total);
            if (observed > shown) {
              expect(text, `${JSON.stringify(count)} hid a shortfall`).toContain(
                `showing ${shown}`,
              );
              expect(text).toContain(`${observed}`);
            }
            if (atLeast) {
              expect(text, `${JSON.stringify(count)} lost its floor marker`).toContain(
                "at least",
              );
            }
            // Whatever else it prints, a bounded count is never just a number.
            if (isBounded(count)) {
              expect(text).not.toMatch(/^\d+$/);
            }
          }
        }
      }
    }
  });
});

describe("isBounded", () => {
  it("is true when rows were cut or the total is a floor", () => {
    expect(isBounded({ shown: 3, total: 12 })).toBe(true);
    expect(isBounded({ shown: 3, total: 3, atLeast: true })).toBe(true);
  });

  it("is false only for a complete, fully displayed result", () => {
    expect(isBounded({ shown: 3, total: 3 })).toBe(false);
    expect(isBounded({ shown: 0, total: 0 })).toBe(false);
  });
});
