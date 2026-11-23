import { describe, expect, it } from "vitest";
import { changedTurns, describeCode, hasAnyChange, parsePatch } from "./changes";
import { applyLine, emptyThread } from "./reduce";

describe("parsePatch", () => {
  it("reads hunks, default line counts and binary markers from git's output", () => {
    const patch = [
      "diff --git a/a.ts b/a.ts",
      "--- a/a.ts",
      "+++ b/a.ts",
      "@@ -3,2 +3,3 @@ fn",
      " keep",
      "-old",
      "+new",
      "+more",
      "\\ No newline at end of file",
      "@@ -10 +11 @@",
      "-x",
      "+y",
    ].join("\n");
    const { hunks, binary } = parsePatch(patch);
    expect(binary).toBe(false);
    expect(hunks).toEqual([
      { oldStart: 3, oldLines: 2, newStart: 3, newLines: 3, lines: [" keep", "-old", "+new", "+more"] },
      { oldStart: 10, oldLines: 1, newStart: 11, newLines: 1, lines: ["-x", "+y"] },
    ]);
    expect(parsePatch("Binary files a/x.png and b/x.png differ")).toEqual({ hunks: [], binary: true });
  });
});

describe("describeCode", () => {
  it("puts a porcelain code in words", () => {
    expect([null, "  ", "??", " D", "A ", "R ", "UU", " M"].map(describeCode)).toEqual([
      "",
      "Back to HEAD",
      "New",
      "Deleted",
      "Added",
      "Renamed",
      "Conflict",
      "Modified",
    ]);
  });
});

describe("changedTurns and hasAnyChange", () => {
  it("list turns that edited or moved files, newest first, and stop at the first change", () => {
    const state = emptyThread();
    expect(hasAnyChange(state)).toBe(false);
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "a", turn: 1 }));
    applyLine(state, JSON.stringify({ type: "assistant", message: { id: "m", content: [{ type: "tool_use", id: "t", name: "Edit", input: { file_path: "/r/a.ts", old_string: "a", new_string: "b" } }] } }));
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "b", turn: 2 }));
    applyLine(state, JSON.stringify({ type: "mavi_user", text: "c", turn: 3 }));
    applyLine(state, JSON.stringify({ type: "mavi_turn_files", turn: 3, available: true, moved: [{ path: "x", code: " M" }] }));
    expect(changedTurns(state)).toEqual([3, 1]);
    expect(hasAnyChange(state)).toBe(true);
  });
});
