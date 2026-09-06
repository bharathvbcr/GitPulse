import { describe, expect, it } from "vitest";
import { describePulseCoverage, fleetLanguageMix, fleetPulse, trendOf, TREND_DAYS } from "./pulse";
import {
  UNSCANNED,
  failedCell,
  readCell,
  type CommitsCellValue,
  type FleetLanguageStat,
  type FleetRow,
} from "./types";

const WINDOW = 90;

function series(entries: Record<number, number> = {}, days = WINDOW): number[] {
  const daily = new Array<number>(days).fill(0);
  // Keys are "buckets back from now", so a test reads as "3 commits two days
  // ago" rather than as an index arithmetic puzzle.
  for (const [back, count] of Object.entries(entries)) {
    daily[days - 1 - Number(back)] = count;
  }
  return daily;
}

function commits(overrides: Partial<CommitsCellValue> = {}): CommitsCellValue {
  const daily = overrides.daily ?? series();
  return {
    windowDays: daily.length,
    commits: daily.reduce((sum, n) => sum + n, 0),
    authors: 1,
    activeDays: daily.filter((n) => n > 0).length,
    recent: 0,
    prior: 0,
    ...overrides,
    daily,
  };
}

function row(overrides: Partial<FleetRow> = {}): FleetRow {
  return {
    path: "/repo/a",
    label: "a",
    presence: "open",
    branch: "main",
    severity: "clean",
    headline: "clean",
    changes: UNSCANNED,
    sync: UNSCANNED,
    watchWarning: null,
    work: UNSCANNED,
    activity: UNSCANNED,
    commits: UNSCANNED,
    loc: UNSCANNED,
    storage: UNSCANNED,
    health: UNSCANNED,
    coverage: UNSCANNED,
    ...overrides,
  };
}

function language(name: string, lines: number, pct = 0): FleetLanguageStat {
  return {
    language: name,
    color_hex: "#123456",
    category: "programming",
    code_lines: lines,
    file_count: 1,
    percentage: pct,
  };
}

describe("trendOf", () => {
  it("refuses a percentage against an empty prior period", () => {
    // "+100%" for one commit after a silent fortnight reads like a finding.
    const trend = trendOf(1, 0);
    expect(trend.deltaPct).toBeNull();
    expect(trend.direction).toBe("new");
  });

  it("separates a fleet that started moving from one that never did", () => {
    expect(trendOf(0, 0).direction).toBe("flat");
    expect(trendOf(3, 0).direction).toBe("new");
  });

  it("reports the signed change when both periods have commits", () => {
    const trend = trendOf(15, 10);
    expect(trend.deltaPct).toBeCloseTo(50);
    expect(trend.direction).toBe("up");
    expect(trendOf(5, 10).direction).toBe("down");
    expect(trendOf(10, 10).direction).toBe("flat");
  });
});

describe("fleetPulse", () => {
  it("sums the series bucket for bucket across repositories", () => {
    const rows = [
      row({ commits: readCell(commits({ daily: series({ 0: 2, 5: 1 }) }), null) }),
      row({ path: "/repo/b", label: "b", commits: readCell(commits({ daily: series({ 0: 3 }) }), null) }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.windowDays).toBe(WINDOW);
    expect(pulse.daily[WINDOW - 1]).toBe(5);
    expect(pulse.daily[WINDOW - 6]).toBe(1);
    expect(pulse.commits).toBe(6);
    expect(pulse.peak).toBe(5);
    expect(pulse.activeDays).toBe(2);
    expect(pulse.counted).toBe(2);
    expect(describePulseCoverage(pulse)).toBe("");
  });

  it("counts a failed window as a shortfall, never as a quiet repository", () => {
    // The whole point. A repository whose history could not be read must not
    // flatten the fleet's chart and then be invisible in the coverage line.
    const rows = [
      row({ commits: readCell(commits({ daily: series({ 0: 4 }) }), null) }),
      row({ path: "/b", label: "b", commits: failedCell("git is not on PATH") }),
      row({ path: "/c", label: "c", commits: UNSCANNED }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.commits).toBe(4);
    expect(pulse.counted).toBe(1);
    expect(pulse.eligible).toBe(3);
    expect(pulse.failed).toBe(1);
    expect(pulse.unscanned).toBe(1);
    expect(describePulseCoverage(pulse)).toContain("counted across 1 of 3");
    expect(describePulseCoverage(pulse)).toContain("1 could not be read");
    expect(describePulseCoverage(pulse)).toContain("1 not swept");
  });

  it("excludes a row whose window does not line up rather than adding it at the wrong offset", () => {
    const rows = [
      row({ commits: readCell(commits({ daily: series({ 0: 2 }) }), null) }),
      row({ path: "/b", label: "b", commits: readCell(commits({ daily: series({ 0: 9 }, 30) }), null) }),
      row({ path: "/c", label: "c", commits: readCell(commits({ daily: series({ 0: 1 }) }), null) }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.windowDays, "the modal window wins").toBe(WINDOW);
    expect(pulse.commits).toBe(3);
    expect(pulse.mismatched).toBe(1);
    expect(describePulseCoverage(pulse)).toContain("1 on a different window");
  });

  it("rejects a series whose length disagrees with its own declared window", () => {
    const broken = { ...commits({ daily: series({ 0: 5 }) }), windowDays: WINDOW, daily: [1, 2, 3] };
    const rows = [
      row({ commits: readCell(commits({ daily: series({ 0: 2 }) }), null) }),
      row({ path: "/b", label: "b", commits: readCell(broken, null) }),
      row({ path: "/c", label: "c", commits: readCell(commits({ daily: series() }), null) }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.mismatched).toBe(1);
    expect(pulse.commits).toBe(2);
  });

  it("carries a capped history through as a floor", () => {
    const rows = [row({ commits: readCell(commits({ daily: series({ 0: 20 }) }), null, true) })];
    const pulse = fleetPulse(rows);
    expect(pulse.partial).toBe(true);
    expect(describePulseCoverage(pulse)).toContain("some histories capped");
  });

  it("draws the trend from the summed series, not from per-repository fields", () => {
    // Two repositories, each active in a different week. Summing their own
    // `recent`/`prior` numbers would double-count the boundary; the fleet
    // trend has to come off the fleet's own series.
    const rows = [
      row({ commits: readCell(commits({ daily: series({ 1: 4 }) }), null) }),
      row({ path: "/b", label: "b", commits: readCell(commits({ daily: series({ 9: 2 }) }), null) }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.trend.recent).toBe(4);
    expect(pulse.trend.prior).toBe(2);
    expect(TREND_DAYS).toBe(7);
  });

  it("names the busiest repositories and the ones that have gone quiet", () => {
    const rows = [
      row({ label: "busy", commits: readCell(commits({ daily: series({ 0: 9 }), recent: 9 }), null) }),
      row({ path: "/b", label: "quiet", commits: readCell(commits({ daily: series() }), null) }),
      row({ path: "/c", label: "some", commits: readCell(commits({ daily: series({ 2: 1 }), recent: 1 }), null) }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.busiest.map((r) => r.label)).toEqual(["busy", "some"]);
    expect(pulse.dormant.map((r) => r.label)).toEqual(["quiet"]);
  });

  it("leaves recents rows out of every number", () => {
    const rows = [
      row({ commits: readCell(commits({ daily: series({ 0: 1 }) }), null) }),
      row({
        path: "/old",
        label: "old",
        presence: "recent",
        commits: readCell(commits({ daily: series({ 0: 99 }) }), null),
      }),
    ];
    const pulse = fleetPulse(rows);
    expect(pulse.commits).toBe(1);
    expect(pulse.eligible).toBe(1);
  });

  it("reports an empty workspace without pretending to have measured it", () => {
    expect(describePulseCoverage(fleetPulse([]))).toBe("no repositories are open");
    const unread = fleetPulse([row({ commits: UNSCANNED })]);
    expect(unread.counted).toBe(0);
    expect(unread.daily).toHaveLength(0);
    expect(describePulseCoverage(unread)).toContain("has not read commit history yet");
  });

  it("says so when nothing could be read at all, rather than drawing calm", () => {
    const pulse = fleetPulse([row({ commits: failedCell("boom") })]);
    expect(pulse.counted).toBe(0);
    expect(pulse.failed).toBe(1);
    expect(describePulseCoverage(pulse)).toContain("no commit history could be read");
  });
});

describe("fleetLanguageMix", () => {
  it("sums lines rather than averaging percentages", () => {
    // A 200-line all-Rust repository and a 200,000-line all-TypeScript one do
    // not make a fleet that is half Rust.
    const rows = [
      row({ loc: readCell({ lines: 200, language: "Rust", languages: [language("Rust", 200, 100)] }, 1) }),
      row({
        path: "/b",
        label: "b",
        loc: readCell(
          { lines: 200_000, language: "TypeScript", languages: [language("TypeScript", 200_000, 100)] },
          1,
        ),
      }),
    ];
    const mix = fleetLanguageMix(rows);
    expect(mix.totalLines).toBe(200_200);
    const rust = mix.stats.find((s) => s.language === "Rust");
    expect(rust?.percentage).toBeCloseTo(0.0999, 3);
    expect(mix.stats[0].language).toBe("TypeScript");
    expect(mix.counted).toBe(2);
  });

  it("merges the same language across repositories into one segment", () => {
    const rows = [
      row({ loc: readCell({ lines: 100, language: "Go", languages: [language("Go", 100)] }, 1) }),
      row({
        path: "/b",
        label: "b",
        loc: readCell({ lines: 300, language: "Go", languages: [language("Go", 300)] }, 1),
      }),
    ];
    const mix = fleetLanguageMix(rows);
    expect(mix.stats).toHaveLength(1);
    expect(mix.stats[0].code_lines).toBe(400);
    expect(mix.stats[0].percentage).toBeCloseTo(100);
  });

  it("counts a scanned repository with no breakdown apart from an unscanned one", () => {
    // The first has a line count on the grid and no mix on file; the second
    // has neither. Reporting them the same would make a rescan look pointless.
    const rows = [
      row({ loc: readCell({ lines: 100, language: "Go", languages: [language("Go", 100)] }, 1) }),
      row({ path: "/b", label: "b", loc: readCell({ lines: 50, language: null, languages: [] }, 1) }),
      row({ path: "/c", label: "c", loc: UNSCANNED }),
    ];
    const mix = fleetLanguageMix(rows);
    expect(mix.counted).toBe(1);
    expect(mix.withoutBreakdown).toBe(1);
    expect(mix.eligible).toBe(3);
  });

  it("carries a capped language scan through as a floor", () => {
    const rows = [
      row({ loc: readCell({ lines: 10, language: "Go", languages: [language("Go", 10)] }, 1, true) }),
    ];
    expect(fleetLanguageMix(rows).partial).toBe(true);
  });
});
