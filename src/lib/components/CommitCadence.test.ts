import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./CommitCadence.svelte", import.meta.url), "utf8");

/**
 * The bucketing itself is covered in metrics/commitCadence.test.ts. What
 * matters here is that the component stays a thin renderer: it must not
 * reimplement bucketing, must expose the summary to assistive tech, and must
 * keep an empty bar visible rather than collapsing to nothing.
 */
describe("CommitCadence component", () => {
  it("delegates bucketing rather than recomputing it", () => {
    expect(source).toContain("bucketCommitsByDay");
    expect(source).toContain("sparklineHeights");
    // no hand-rolled day arithmetic in the view layer
    expect(source).not.toContain("86400");
    expect(source).not.toContain("86_400");
  });

  it("takes `now` as a prop so rendering is deterministic under test", () => {
    expect(source).toMatch(/now\?: number/);
  });

  it("describes itself to assistive tech", () => {
    expect(source).toContain('role="img"');
    expect(source).toContain("aria-label={label}");
  });

  it("keeps every bar at least one pixel so an empty day is still drawn", () => {
    expect(source).toContain("Math.max(1,");
  });

  it("says when the window exceeds the loaded history", () => {
    expect(source).toContain("summary.partial");
    expect(source).toContain("whole loaded history");
  });
});
