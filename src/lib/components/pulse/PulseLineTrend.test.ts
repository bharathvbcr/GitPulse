import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { render } from "svelte/server";
import PulseLineTrend from "./PulseLineTrend.svelte";
import type { PulseCommitSummary } from "../../pulse/types";

const source = readFileSync(new URL("./PulseLineTrend.svelte", import.meta.url), "utf8");

/**
 * The LOC anchor comes from a scan that can be partial or fail outright. A
 * failed scan rendering as "0 LOC" is the exact honesty bug this view was
 * fixed for, so each state must stay separately rendered.
 */
describe("PulseLineTrend component", () => {
  it("delegates both series to tested metrics", () => {
    expect(source).toContain("computeLineChanges");
    expect(source).toContain("computeLocTrend");
  });

  it("distinguishes every LOC scan state", () => {
    expect(source).toMatch(/locStatus\?: "idle" \| "loading" \| "ok" \| "partial" \| "failed"/);
    expect(source).toContain('locStatus === "failed"');
    expect(source).toContain('locStatus === "loading"');
    expect(source).toContain('"partial"');
  });

  it("only reconstructs the LOC trend when a total is actually known", () => {
    expect(source).toMatch(/locStatus === "ok" \|\| locStatus === "partial"\s*\?\s*computeLocTrend/);
  });

  it("labels each weekly column for assistive tech", () => {
    expect(source).toContain('role="img"');
    expect(source).toContain("aria-label=");
  });
});

/**
 * The headline sits beside "(Past 26 Weeks)" and a 26-bar chart, so it has to
 * be the sum of those bars. It used to sum every scanned commit, up to 25,000
 * of them, with nothing on screen saying so.
 */
describe("PulseLineTrend totals", () => {
  const NOW = new Date(2026, 9, 28, 12).getTime();
  const DAY = 86_400;

  function commit(daysAgo: number, additions: number, deletions: number): PulseCommitSummary {
    return {
      sha: `sha-${daysAgo}-${additions}`,
      parents: [],
      timestamp: Math.floor(NOW / 1000) - daysAgo * DAY,
      summary: "change",
      author_name: "Ada",
      author_email: "ada@example.test",
      signature: "none",
      additions,
      deletions,
      files_changed: 1,
      is_merge: false,
      is_revert: false,
      co_authors: [],
      binary_files: 0,
    } as PulseCommitSummary;
  }

  /** Whitespace-normalised visible text. */
  function text(commits: PulseCommitSummary[]): string {
    const { body } = render(PulseLineTrend, { props: { commits, now: NOW } });
    return body.replace(/<!--.*?-->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
  }

  // Two commits inside the 26-week window, one 30 weeks back that the chart cannot show.
  const history = [commit(1, 100, 40), commit(14, 50, 10), commit(210, 900, 400)];

  it("sums the headline over the weeks the chart draws, not the whole scan", () => {
    const rendered = text(history);
    expect(rendered).toContain("+150");
    expect(rendered).toContain("-50");
    expect(rendered).toContain("Net +100");
    // The old-commit lines are not in the headline.
    expect(rendered).not.toContain("Net +610");
  });

  it("keeps the whole scanned history as a separately labelled figure", () => {
    expect(text(history)).toContain("All 3 scanned commits: +1,050 / −450");
  });

  it("agrees with itself when nothing is older than the window", () => {
    const rendered = text([commit(1, 100, 40)]);
    expect(rendered).toContain("Net +60");
    expect(rendered).toContain("All 1 scanned commit: +100 / −40");
  });
});
