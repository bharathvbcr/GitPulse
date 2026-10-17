import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Every file the frontend coverage floor ignores is a hole in it. A hole with
 * no name and no date is permanent: "still gaining unit tests" stood on
 * `repoMetrics.ts` and `graphStore.ts` for months while the floor was reported
 * as honest. Each exclusion therefore carries `owner: <who>; review by
 * <YYYY-MM-DD>`, and a date that has passed fails this test until someone
 * decides — on purpose — to move it.
 */
const CONFIG = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "vitest.config.ts");

/** Test files and test directories are not code under test; they need no owner. */
const TEST_GLOBS = new Set(["src/**/*.test.ts", "src/**/*.spec.ts", "src/**/__tests__/**"]);

function parseExclusions(source: string) {
  const block = /exclude:\s*\[([\s\S]*?)\n\s*\],/.exec(source);
  if (!block) throw new Error("vitest.config.ts has no coverage exclude list");
  const entries: Array<{ glob: string; comment: string }> = [];
  for (const line of block[1]!.split("\n")) {
    const match = /^\s*"([^"]+)",?\s*(?:\/\/\s*(.*))?$/.exec(line);
    if (match) entries.push({ glob: match[1]!, comment: match[2] ?? "" });
  }
  return entries;
}

describe("the coverage exclusion list", () => {
  const entries = parseExclusions(readFileSync(CONFIG, "utf8"));
  const code = entries.filter((entry) => !TEST_GLOBS.has(entry.glob));

  it("is parsed from the real config, not an empty one", () => {
    expect(entries.length).toBeGreaterThan(TEST_GLOBS.size);
    expect(code.map((entry) => entry.glob)).toContain("**/*.svelte");
  });

  it("no longer hides the two files the floor was claiming to cover", () => {
    const globs = code.map((entry) => entry.glob);
    expect(globs).not.toContain("src/lib/stores/graphStore.ts");
    expect(globs).not.toContain("src/lib/metrics/repoMetrics.ts");
  });

  it.each(code.map((entry) => [entry.glob, entry.comment] as const))("%s names an owner and a review date", (_glob, comment) => {
    const match = /owner:\s*([^;]+);\s*review by\s*(\d{4}-\d{2}-\d{2})/.exec(comment);
    expect(match, `no "owner: <who>; review by <YYYY-MM-DD>" in "${comment}"`).not.toBeNull();
    expect(match![1]!.trim()).not.toBe("");
    const due = new Date(`${match![2]}T00:00:00Z`);
    expect(Number.isNaN(due.getTime()), `${match![2]} is not a date`).toBe(false);
    expect(due.getTime(), `review date ${match![2]} has passed; look at this exclusion again`).toBeGreaterThan(Date.now());
  });

  it("reads an entry without a comment as having none, which the check above then refuses", () => {
    const parsed = parseExclusions('exclude: [\n  "src/x.ts",\n ],\n');
    expect(parsed).toEqual([{ glob: "src/x.ts", comment: "" }]);
  });
});
