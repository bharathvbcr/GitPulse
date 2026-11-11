import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { render } from "svelte/server";
import PulseCodeAge from "./PulseCodeAge.svelte";
import PulseDora from "./PulseDora.svelte";
import PulseKnowledgeMap from "./PulseKnowledgeMap.svelte";
import type { DoraReport, KnowledgeReport } from "../../pulse/types";

/**
 * A secondary refresh (Re-scan Blame / Refresh DORA) that fails after an
 * earlier success leaves the old figures on screen. The panels used to render
 * the error only when there was no value at all, so those figures read as
 * current. Each panel must say the refresh failed, and how old the figures are.
 */

const knowledge: KnowledgeReport = {
  scanned_files: 12,
  candidate_files: 12,
  scanned_lines: 900,
  bus_factor: 2,
  primary_authors: [],
  orphaned_files: [],
  age_distribution: {
    fresh_lines: 900,
    recent_lines: 0,
    maturing_lines: 0,
    legacy_lines: 0,
    ancient_lines: 0,
    total_lines: 900,
  },
  half_life_days: 40,
  truncated: false,
  duration_ms: 5,
  areas: [],
  areas_total: 0,
  churn_window_capped: false,
  churn_unavailable: false,
  submodules_skipped: 0,
};

const dora: DoraReport = {
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
};

type Props = { error?: string | null; measuredAt?: number | null };

const panels: ReadonlyArray<{
  name: string;
  html: (props: Props) => string;
  stillShown: string;
}> = [
  {
    name: "PulseKnowledgeMap",
    html: (props) => render(PulseKnowledgeMap, { props: { knowledge, ...props } }).body,
    stillShown: "Bus factor",
  },
  {
    name: "PulseCodeAge",
    html: (props) => render(PulseCodeAge, { props: { knowledge, ...props } }).body,
    stillShown: "Repository code half-life",
  },
  {
    name: "PulseDora",
    html: (props) => render(PulseDora, { props: { dora, ...props } }).body,
    stillShown: "Deploy frequency",
  },
];

describe.each(panels)("$name stale-on-failure notice", ({ html, stillShown }) => {
  it("says the last refresh failed and keeps the previous figures on screen", () => {
    const body = html({ error: "blame timed out", measuredAt: Date.now() - 5 * 60_000 });
    expect(body).toContain('role="status"');
    expect(body).toContain(
      "Couldn't refresh the scan. blame timed out. Showing the previous scan from 5m ago.",
    );
    expect(body).toContain(stillShown);
  });

  it("omits the age clause when no measurement time is known", () => {
    const body = html({ error: "blame timed out", measuredAt: null });
    expect(body).toContain("Couldn't refresh the scan. blame timed out. Showing the previous scan.");
    expect(body).not.toContain("scan from");
  });

  it("stays silent when the last refresh succeeded", () => {
    const body = html({ error: null, measuredAt: Date.now() });
    expect(body).not.toContain("Couldn't refresh the scan");
    expect(body).toContain(stillShown);
  });
});

describe("PulseStaleNotice wording", () => {
  const notice = (props: Props) => render(PulseKnowledgeMap, { props: { knowledge, ...props } }).body;

  it("does not claim a scan from 'just now' that can age while the notice is on screen", () => {
    const body = notice({ error: "blame timed out", measuredAt: Date.now() - 10_000 });
    expect(body).toContain("Showing the previous scan from moments ago.");
    expect(body).not.toContain("just now");
  });

  it("does not double the full stop when the error already ends in one", () => {
    const body = notice({ error: "blame timed out.", measuredAt: null });
    expect(body).toContain("Couldn't refresh the scan. blame timed out. Showing the previous scan.");
    expect(body).not.toContain("..");
  });
});

describe("knowledge tab announces a failed refresh once", () => {
  const props = { knowledge, error: "blame timed out", measuredAt: Date.now() - 5 * 60_000 };
  const occurrences = (body: string) => body.split("Couldn't refresh the scan").length - 1;

  it("PulseCodeAge can leave the notice to the card above it", () => {
    const body = render(PulseCodeAge, { props: { ...props, staleNotice: false } }).body;
    expect(occurrences(body)).toBe(0);
    expect(body).toContain("Repository code half-life");
  });

  it("PulseCodeAge keeps its own notice when used on its own", () => {
    expect(occurrences(render(PulseCodeAge, { props }).body)).toBe(1);
  });

  it("PulseView turns the second copy off", () => {
    const view = readFileSync(new URL("./PulseView.svelte", import.meta.url), "utf8");
    const codeAge = view.slice(view.indexOf("<PulseCodeAge"));
    expect(codeAge.slice(0, codeAge.indexOf("/>"))).toContain("staleNotice={false}");
  });

  it("PulseView scopes the line-change totals to the selected author", () => {
    const view = readFileSync(new URL("./PulseView.svelte", import.meta.url), "utf8");
    const lineTrend = view.slice(view.indexOf("<PulseLineTrend"));
    expect(lineTrend.slice(0, lineTrend.indexOf("/>"))).toContain('scoped={authorFilter !== "all"}');
  });
});
