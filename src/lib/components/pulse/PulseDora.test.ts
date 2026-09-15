import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { render } from "svelte/server";
import PulseDora from "./PulseDora.svelte";
import type { DoraReport } from "../../pulse/types";

const source = readFileSync(new URL("./PulseDora.svelte", import.meta.url), "utf8");

/**
 * A report whose every other number is non-zero, so "0%" and "—" in the body
 * can only have come from the change-failure card.
 */
function doraReport(overrides: Partial<DoraReport> = {}): DoraReport {
  return {
    deploy_frequency_per_week: 2.5,
    deploy_rating: "High",
    total_releases: 12,
    median_lead_time_hours: 4.5,
    lead_time_rating: "Elite",
    change_failure_rate_pct: 7.5,
    is_cfr_approximation: true,
    cfr_sample_commits: 160,
    commit_scan_truncated: false,
    commit_scan_window_commits: null,
    mttr_hours: 2,
    is_mttr_approximation: true,
    window_days: 90,
    ...overrides,
  };
}

function body(overrides: Partial<DoraReport> = {}): string {
  return render(PulseDora, { props: { dora: doraReport(overrides) } }).body;
}

/**
 * Two of the four DORA numbers are heuristics derived from commit patterns.
 * Presenting them with the same confidence as the tag-derived two would be the
 * "unexamined looks like verified" failure, so the view must label them.
 */
describe("PulseDora component", () => {
  it("shows a loading state instead of an empty scorecard", () => {
    expect(source).toMatch(/loading\?: boolean/);
    expect(source).toContain("{#if loading && !dora}");
  });

  it("marks the approximated metrics as approximations", () => {
    expect(source).toContain("is_mttr_approximation");
    expect(source).toContain("heuristic");
  });

  it("declines to invent a restore time when there were no samples", () => {
    expect(source).toContain("Could not estimate from commit patterns");
    expect(source).toMatch(/is_mttr_approximation && dora\.mttr_hours <= 0/);
  });

  it("names the git commands the measured metrics come from", () => {
    expect(source).toContain("git describe --contains");
  });
});

describe("PulseDora failure state", () => {
  const source = readFileSync(new URL("./PulseDora.svelte", import.meta.url), "utf8");

  it("renders a failed delivery scan distinctly from zero deploys", () => {
    expect(source).toMatch(/error\?: string \| null/);
    expect(source).toContain("{:else if error && !dora}");
    expect(source).toContain("not a delivery frequency of zero");
  });
});

/**
 * The change-failure rate is the one card whose value cannot double as its own
 * "not measured" sentinel: 0% is a legitimate answer when commits were
 * examined and none were reverts. `is_cfr_approximation` cannot carry the
 * distinction either — both construction sites set it unconditionally, so it
 * says "heuristic", never "nothing to measure". Only the denominator can, and
 * a check that could not run must never render the same as one that ran.
 */
describe("PulseDora change failure rate", () => {
  it("renders a real measured zero as 0%", () => {
    const rendered = body({ change_failure_rate_pct: 0, cfr_sample_commits: 137 });
    expect(rendered).toContain("0%");
    expect(rendered).toContain("137 examined commits");
    expect(rendered).not.toContain("No commits in this window to examine");
  });

  it("declines to render an empty window as 0%", () => {
    const rendered = body({ change_failure_rate_pct: 0, cfr_sample_commits: 0 });
    expect(rendered).not.toContain("0%");
    expect(rendered).toContain("No commits in this window to examine");
    expect(rendered).toContain("—");
  });

  it("does not present an empty window as clean even when the rate is non-zero", () => {
    // Defensive: a future caller that forgets to zero the rate alongside the
    // sample must still not get a confident number out of an empty scan.
    const rendered = body({ change_failure_rate_pct: 7.5, cfr_sample_commits: 0 });
    expect(rendered).not.toContain("7.5%");
    expect(rendered).toContain("No commits in this window to examine");
  });

  it("names the sample the rate was measured over", () => {
    expect(body({ cfr_sample_commits: 160 })).toContain("160 examined commits");
    // `toContain` alone would pass on "1 examined commits", so pin the plural
    // out as well rather than letting a substring match stand in for the text.
    const single = body({ cfr_sample_commits: 1 });
    expect(single).toContain("1 examined commit");
    expect(single).not.toContain("examined commits");
  });
});

/**
 * The sample size says "measured" versus "nothing to measure". It cannot say
 * whether the scan covered the window, because 200 examined is the same number
 * whether the window held 200 or 532. Card 1 reports release tags over the
 * whole window beside these two, so a window the scan only partly read must
 * say so on the tiles it actually applies to — and it applies to both, because
 * the rate and the restore time come from one capped `git log`.
 */
describe("PulseDora commit-scan truncation", () => {
  const truncated = {
    cfr_sample_commits: 200,
    commit_scan_truncated: true,
    commit_scan_window_commits: 532,
  };

  it("says the change-failure rate covered only part of the window", () => {
    const rendered = body(truncated);
    expect(rendered).toContain("200 examined commits");
    expect(rendered).toContain("the newest of 532 in the window, not all of it");
  });

  it("carries the same caveat on the restore time", () => {
    // One capped log feeds both metrics, so a caveat on the rate alone would
    // leave the restore time reading as if it had seen the whole window.
    const rendered = body({ ...truncated, mttr_hours: 6 });
    const occurrences = rendered.split("not all of it").length - 1;
    expect(
      occurrences,
      "both the change-failure and restore tiles must carry the caveat",
    ).toBe(2);
    expect(rendered).toContain("Time to follow-up patch (heuristic)");
  });

  it("stays silent when the scan read the whole window", () => {
    // Without this the caveat could be unconditional, which would make it
    // meaningless — every report would warn and none would inform.
    const rendered = body({ cfr_sample_commits: 160, commit_scan_truncated: false });
    expect(rendered).not.toContain("not all of it");
    expect(rendered).toContain("160 examined commits");
  });

  it("still reports truncation when the window total could not be read", () => {
    // `commit_scan_window_commits` is null when the uncapped count failed.
    // The scan was still cut, so silence here would let a partial answer
    // render exactly like a complete one — the failure the flag exists for.
    const rendered = body({
      cfr_sample_commits: 200,
      commit_scan_truncated: true,
      commit_scan_window_commits: null,
    });
    expect(rendered).toContain("the newest in the window, not all of it");
    // No fabricated total, and no stray "of undefined".
    expect(rendered).not.toContain("undefined");
  });
});
