import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./wordDiff";
import { buildFilePatchForHunk, buildFilePatchFromLines, parseHunkHeaderNumbers } from "./patchBuilder";

const sample = [
  "diff --git a/src/a.rs b/src/a.rs",
  "--- a/src/a.rs",
  "+++ b/src/a.rs",
  "@@ -1,4 +1,5 @@",
  " fn main() {",
  "-    old();",
  "+    new();",
  "+    extra();",
  " }",
].join("\n");

describe("parseHunkHeaderNumbers", () => {
  it("reads both sides and defaults omitted counts to 1", () => {
    expect(parseHunkHeaderNumbers("@@ -3,4 +9,2 @@ fn main()")).toEqual({
      old_start: 3,
      old_lines: 4,
      new_start: 9,
      new_lines: 2,
    });
    expect(parseHunkHeaderNumbers("@@ -1 +1 @@")).toEqual({
      old_start: 1,
      old_lines: 1,
      new_start: 1,
      new_lines: 1,
    });
  });
});

describe("buildFilePatchFromLines", () => {
  it("serializes selected add/del lines into a backend FilePatch", () => {
    const lines = parseUnifiedDiff(sample);
    const add = lines.findIndex((l) => l.content.includes("new();"));
    const extra = lines.findIndex((l) => l.content.includes("extra();"));
    const patch = buildFilePatchFromLines(lines, "src/a.rs", new Set([add, extra]));
    expect(patch).not.toBeNull();
    expect(patch?.old_path).toBe("src/a.rs");
    expect(patch?.hunks).toHaveLength(1);
    const selected = patch!.hunks[0].lines.filter((l) => l.is_selected);
    expect(selected.map((l) => l.content)).toEqual(["    new();", "    extra();"]);
    expect(selected.every((l) => l.line_type === "Addition")).toBe(true);
  });

  it("returns null when no add/del lines are selected", () => {
    const lines = parseUnifiedDiff(sample);
    expect(buildFilePatchFromLines(lines, "src/a.rs", new Set())).toBeNull();
    const ctx = lines.findIndex((l) => l.type === "ctx");
    expect(buildFilePatchFromLines(lines, "src/a.rs", new Set([ctx]))).toBeNull();
  });
});

describe("buildFilePatchForHunk", () => {
  it("selects every add/del in the hunk that starts at the header index", () => {
    const lines = parseUnifiedDiff(sample);
    const header = lines.findIndex((l) => l.type === "hdr" && l.content.startsWith("@@"));
    const patch = buildFilePatchForHunk(lines, "src/a.rs", header);
    expect(patch).not.toBeNull();
    const selected = patch!.hunks[0].lines.filter((l) => l.is_selected).map((l) => l.content);
    expect(selected).toEqual(["    old();", "    new();", "    extra();"]);
  });
});
