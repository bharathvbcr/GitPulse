import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { render } from "svelte/server";
import PulsePeriodCompare from "./PulsePeriodCompare.svelte";
import type { PulseCommitSummary } from "../../pulse/types";

const source = readFileSync(new URL("./PulsePeriodCompare.svelte", import.meta.url), "utf8");

describe("PulsePeriodCompare component", () => {
  it("delegates the period arithmetic to the tested metric", () => {
    expect(source).toContain("computePeriodCompare");
    expect(source).not.toContain("86400");
  });

  it("is told whether the scan was cut, as the rhythm strip is", () => {
    const view = readFileSync(new URL("./PulseView.svelte", import.meta.url), "utf8");
    expect(view).toContain("<PulsePeriodCompare commits={visibleCommits} truncated={report.truncated} />");
  });
});

/**
 * Growth from zero used to render as "+100%" with an up arrow, including when
 * the scan was cut before the prior period so "zero" only meant "not read".
 */
describe("PulsePeriodCompare without a prior period to compare with", () => {
  const DAY_MS = 86_400_000;

  function commit(daysAgo: number): PulseCommitSummary {
    return {
      sha: `sha-${daysAgo}`,
      parents: [],
      timestamp: Math.floor((Date.now() - daysAgo * DAY_MS) / 1000),
      summary: "change",
      author_name: "Ada",
      author_email: "ada@example.test",
      signature: "none",
      additions: 30,
      deletions: 10,
      files_changed: 1,
      is_merge: false,
      is_revert: false,
      co_authors: [],
      binary_files: 0,
    } as PulseCommitSummary;
  }

  const text = (props: { commits: PulseCommitSummary[]; truncated?: boolean }): string =>
    render(PulsePeriodCompare, { props }).body.replace(/<!--.*?-->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");

  it("shows a neutral \"new\" rather than +100% when the prior period is empty", () => {
    const rendered = text({ commits: [commit(3)] });
    expect(rendered).not.toContain("100%");
    // Commits, adds and dels all have activity now and nothing before.
    expect(rendered.match(/\bnew\b/g)).toHaveLength(3);
  });

  it("explains what \"new\" means on hover", () => {
    const { body } = render(PulsePeriodCompare, { props: { commits: [commit(3)] } });
    expect(body).toContain('title="No commits in the prior 30 days"');
  });

  it("shows a dash when neither period has activity", () => {
    const rendered = text({ commits: [] });
    expect(rendered).not.toContain("new");
    expect(rendered.match(/—/g)).toHaveLength(3);
  });

  it("still shows a measured percentage when the prior period had commits", () => {
    const rendered = text({ commits: [commit(3), commit(4), commit(40)] });
    expect(rendered).toContain("+100%");
    expect(rendered).not.toMatch(/\bnew\b/);
  });

  it("says the prior period may be outside the scanned window only when the scan was cut inside it", () => {
    const cut = text({ commits: [commit(3), commit(45)], truncated: true });
    expect(cut).toContain("Prior period may be outside the scanned window");
    const reached = text({ commits: [commit(3), commit(75)], truncated: true });
    expect(reached).not.toContain("outside the scanned window");
    const complete = text({ commits: [commit(3)] });
    expect(complete).not.toContain("outside the scanned window");
  });
});
