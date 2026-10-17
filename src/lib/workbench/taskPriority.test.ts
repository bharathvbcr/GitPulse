import { describe, expect, it } from "vitest";
import cases from "./priority.cases.json" with { type: "json" };
import { DEFAULT_PRIORITY, PRIORITY_LABELS, numericPriority, parsePriority, priorityLabel } from "./taskPriority";

describe("parsePriority", () => {
  it("has enough shared cases to matter", () => {
    expect(cases.cases.length).toBeGreaterThanOrEqual(25);
  });

  for (const item of cases.cases) {
    it(`${JSON.stringify(item.input)} -> ${item.expected}`, () => {
      expect(parsePriority(item.input)).toBe(item.expected);
    });
  }

  it("takes stored numbers 0-3 and nothing else that is not a string", () => {
    expect(parsePriority(2)).toBe(2);
    for (const bad of [4, -1, 1.5, Number.NaN, true, null, undefined, {}, [1]]) expect(parsePriority(bad)).toBeNull();
  });
});

describe("numericPriority", () => {
  it("accepts numbers 0-3 and their decimal strings", () => {
    expect([0, 1, 2, 3, "0", "1", "2", "3"].map(numericPriority)).toEqual([0, 1, 2, 3, 0, 1, 2, 3]);
  });
  it("refuses everything else", () => {
    for (const bad of [true, false, 1.5, -1, 4, "4", "", " 1", "01", "high", "p1", Number.NaN, null, undefined, [], [1]]) {
      expect(numericPriority(bad)).toBeNull();
    }
  });
});

describe("labels", () => {
  it("names the scale and defaults to Normal", () => {
    expect(PRIORITY_LABELS).toEqual(["Urgent", "High", "Normal", "Low"]);
    expect(DEFAULT_PRIORITY).toBe(2);
    expect(priorityLabel(undefined)).toBe("Normal");
    expect(priorityLabel(0)).toBe("Urgent");
    expect(priorityLabel(true)).toBe("Normal");
  });
});
