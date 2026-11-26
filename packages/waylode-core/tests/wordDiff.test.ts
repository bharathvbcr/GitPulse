import { describe, expect, it } from "vitest";
import {
  annotateAround,
  annotateUnifiedDiff,
  computeWordDiff,
  createParseCache,
  parseUnifiedDiff,
} from "../src/diff/wordDiff";

describe("packages/waylode-core wordDiff", () => {
  it("computes intra-line token diffs cleanly", () => {
    const diff = computeWordDiff("const count = 1;", "const total = 2;");
    expect(diff.original_segments).toEqual([
      { kind: "Equal", text: "const " },
      { kind: "Removed", text: "count" },
      { kind: "Equal", text: " = " },
      { kind: "Removed", text: "1" },
      { kind: "Equal", text: ";" },
    ]);
    expect(diff.modified_segments).toEqual([
      { kind: "Equal", text: "const " },
      { kind: "Added", text: "total" },
      { kind: "Equal", text: " = " },
      { kind: "Added", text: "2" },
      { kind: "Equal", text: ";" },
    ]);
  });

  it("parses unified diffs with line numbers, EOF no-newline markers, and matches windowed annotation", () => {
    const raw = [
      "diff --git a/a.ts b/a.ts",
      "index 111..222 100644",
      "--- a/a.ts",
      "+++ b/a.ts",
      "@@ -10,2 +10,2 @@",
      "-const a = 1;",
      "+const a = 2;",
      " const b = 3;",
      "\\ No newline at end of file",
      "",
    ].join("\n");

    const parsed = parseUnifiedDiff(raw);
    const delIdx = parsed.findIndex((l) => l.type === "del");
    const ctxIdx = parsed.findIndex((l) => l.type === "ctx");
    expect(parsed[delIdx].oldNo).toBe(10);
    expect(parsed[ctxIdx].noNewline).toBe(true);

    annotateAround(parsed, delIdx);
    const oracle = annotateUnifiedDiff(raw);
    expect(parsed[delIdx].segments).toEqual(oracle[delIdx].segments);
    expect(parsed[delIdx + 1].segments).toEqual(oracle[delIdx + 1].segments);

    const cache = createParseCache();
    expect(cache.parse(raw)).toBe(cache.parse(raw));
  });
});
