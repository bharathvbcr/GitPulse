import { describe, expect, it } from "vitest";
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
    stillShown: "Bus Factor",
  },
  {
    name: "PulseCodeAge",
    html: (props) => render(PulseCodeAge, { props: { knowledge, ...props } }).body,
    stillShown: "Repo Code Half-Life",
  },
  {
    name: "PulseDora",
    html: (props) => render(PulseDora, { props: { dora, ...props } }).body,
    stillShown: "Deploy Frequency",
  },
];

describe.each(panels)("$name stale-on-failure notice", ({ html, stillShown }) => {
  it("says the last refresh failed and keeps the previous figures on screen", () => {
    const body = html({ error: "blame timed out", measuredAt: Date.now() - 5 * 60_000 });
    expect(body).toContain('role="status"');
    expect(body).toContain(
      "Last refresh failed: blame timed out. Showing the previous scan from 5m ago.",
    );
    expect(body).toContain(stillShown);
  });

  it("omits the age clause when no measurement time is known", () => {
    const body = html({ error: "blame timed out", measuredAt: null });
    expect(body).toContain("Last refresh failed: blame timed out. Showing the previous scan.");
    expect(body).not.toContain("scan from");
  });

  it("stays silent when the last refresh succeeded", () => {
    const body = html({ error: null, measuredAt: Date.now() });
    expect(body).not.toContain("Last refresh failed");
    expect(body).toContain(stillShown);
  });
});
