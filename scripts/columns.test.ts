import { describe, expect, it } from "vitest";
import { alignFlags, alignRows } from "./columns.mjs";

/** The column each row's value starts at. */
function valueColumns(lines: string[], separator = ": "): number[] {
  return lines.map((line) => line.indexOf(separator) + separator.length);
}

describe("aligned columns", () => {
  it("puts every value in the same column", () => {
    const lines = alignRows([
      { label: "short", value: "1" },
      { label: "a considerably longer label", value: "2" },
      { label: "mid", value: "3" },
    ]);
    expect(new Set(valueColumns(lines)).size).toBe(1);
  });

  it("re-aligns when a longer label is added rather than needing hand-counted padding", () => {
    const before = alignRows([{ label: "one", value: "1" }, { label: "two", value: "2" }]);
    const after = alignRows([
      { label: "one", value: "1" },
      { label: "two", value: "2" },
      { label: "a much longer third label", value: "3" },
    ]);
    expect(new Set(valueColumns(after)).size).toBe(1);
    expect(valueColumns(after)[0]).toBeGreaterThan(valueColumns(before)[0]);
  });

  it("keeps alignment when a value or note is very long", () => {
    const lines = alignRows([
      { label: "handlers", value: "9".repeat(40), note: "x".repeat(120) },
      { label: "commands", value: "1" },
    ]);
    expect(new Set(valueColumns(lines)).size).toBe(1);
  });

  it("renders a note in parentheses only when present", () => {
    const [withNote, withoutNote] = alignRows([
      { label: "a", value: "1", note: "why" },
      { label: "b", value: "2" },
    ]);
    expect(withNote).toContain("(why)");
    expect(withoutNote).not.toContain("(");
  });

  it("aligns flag descriptions the same way", () => {
    const lines = alignFlags([
      { flag: "--a", description: "short" },
      { flag: "--a-very-long-flag-name", description: "long" },
    ]);
    const starts = lines.map((line) => line.search(/\S+$/));
    expect(new Set(starts).size).toBe(1);
  });

  it("handles an empty row list without throwing", () => {
    expect(alignRows([])).toEqual([]);
    expect(alignFlags([])).toEqual([]);
  });
});
