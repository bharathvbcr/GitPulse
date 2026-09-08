import { describe, expect, it } from "vitest";
import { conflictComparisonRows } from "./conflictPresentation";

describe("conflict comparison", () => {
  it.each([
    ["const interval = 5000;", "const interval = 2500;"],
    ["", "new line\n"],
    ["a\n\nλ🦀", "a\nchanged"],
    ["heading\n=======", "heading\ntext"],
  ])("highlights while preserving both complete sources: %j / %j", (ours, theirs) => {
    const rows = conflictComparisonRows(ours, theirs)!;
    const restored = (side: "ours" | "theirs", source: string) => rows.slice(0, source.split("\n").length).map(row => row[side].map(segment => segment.text).join("")).join("\n");
    expect(restored("ours", ours)).toBe(ours); expect(restored("theirs", theirs)).toBe(theirs);
  });
  it("uses full plain lines when word comparison would be too large", () => {
    const ours = "a".repeat(2001); const theirs = "b".repeat(2001);
    expect(conflictComparisonRows(ours, theirs)).toEqual([{ ours: [{ kind: "Removed", text: ours }], theirs: [{ kind: "Added", text: theirs }] }]);
  });
  it("refuses expensive comparison at explicit block and line limits", () => {
    expect(conflictComparisonRows("x".repeat(40_001), "")).toBeNull();
    expect(conflictComparisonRows("x\n".repeat(200), "")).toBeNull();
    expect(conflictComparisonRows(Array(200).fill("x").join("\n"), "")).toHaveLength(200);
  });
});
