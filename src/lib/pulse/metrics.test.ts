import { describe, expect, it } from "vitest";
import {
  computeHeatmap,
  computeHotspotRisks,
  computeHygiene,
  computeLineChanges,
  computeLocTrend,
  computePeriodCompare,
  computePunchCard,
  computeRhythm,
  formatLocalDayKey,
  isConventionalCommit,
} from "./metrics";
import { generatePulseSvgCard } from "./exportCard";
import type { PulseCommitSummary } from "./types";

function createMockCommit(overrides: Partial<PulseCommitSummary> = {}): PulseCommitSummary {
  return {
    sha: "abc1234567890",
    parents: ["def123"],
    timestamp: 1700000000,
    summary: "feat(core): implement pulse metrics",
    author_name: "Alice Dev",
    author_email: "alice@example.com",
    gpg_status: "G",
    additions: 20,
    deletions: 5,
    files_changed: 3,
    is_merge: false,
    is_revert: false,
    ...overrides,
  };
}

describe("isConventionalCommit", () => {
  it("recognizes standard conventional commits", () => {
    expect(isConventionalCommit("feat: new feature")).toBe(true);
    expect(isConventionalCommit("fix(parser): resolve buffer overflow")).toBe(true);
    expect(isConventionalCommit("docs: update README")).toBe(true);
    expect(isConventionalCommit("refactor(engine)!: break API")).toBe(true);
    expect(isConventionalCommit("chore: bump deps")).toBe(true);
  });

  it("rejects non-conventional commit messages", () => {
    expect(isConventionalCommit("WIP: working on stuff")).toBe(false);
    expect(isConventionalCommit("fixed bug in UI")).toBe(false);
    expect(isConventionalCommit("update styles")).toBe(false);
    expect(isConventionalCommit("")).toBe(false);
  });
});

describe("computeHeatmap", () => {
  it("handles empty commits list gracefully", () => {
    const fixedNow = new Date(2026, 8, 2, 12, 0, 0).getTime();
    const weeks = computeHeatmap([], 4, fixedNow);
    expect(weeks.length).toBe(4);
    for (const w of weeks) {
      expect(w.days.length).toBe(7);
      for (const d of w.days) {
        expect(d!.count).toBe(0);
        expect(d!.level).toBe(0);
      }
    }
  });

  it("buckets commits into the correct day and computes levels", () => {
    const fixedNow = new Date(2026, 8, 2, 12, 0, 0).getTime();
    const commitTime = Math.floor(fixedNow / 1000);
    const commits = [
      createMockCommit({ timestamp: commitTime, additions: 100, deletions: 20 }),
      createMockCommit({ timestamp: commitTime, additions: 50, deletions: 10 }),
    ];

    const weeks = computeHeatmap(commits, 2, fixedNow, "count");
    expect(weeks.length).toBe(2);

    const todayKey = formatLocalDayKey(fixedNow);
    let found = false;
    for (const w of weeks) {
      for (const d of w.days) {
        if (d!.date === todayKey) {
          found = true;
          expect(d!.count).toBe(2);
          expect(d!.additions).toBe(150);
          expect(d!.deletions).toBe(30);
          expect(d!.churn).toBe(180);
          expect(d!.level).toBeGreaterThan(0);
        }
      }
    }
    expect(found).toBe(true);
  });
});

describe("computeRhythm", () => {
  it("returns zero stats for empty commits", () => {
    const rhythm = computeRhythm([], 90);
    expect(rhythm.currentStreak).toBe(0);
    expect(rhythm.longestStreak).toBe(0);
    expect(rhythm.activeDaysInWindow).toBe(0);
  });

  it("calculates active streaks and gaps correctly", () => {
    const fixedNow = new Date(2026, 8, 2, 12, 0, 0).getTime();
    const oneDay = 24 * 60 * 60 * 1000;

    // Commits on today, yesterday, and 2 days ago (streak of 3)
    // Plus a commit 10 days ago (gap of 7 days)
    const t0 = Math.floor(fixedNow / 1000);
    const t1 = Math.floor((fixedNow - oneDay) / 1000);
    const t2 = Math.floor((fixedNow - 2 * oneDay) / 1000);
    const t10 = Math.floor((fixedNow - 10 * oneDay) / 1000);

    const commits = [
      createMockCommit({ timestamp: t0 }),
      createMockCommit({ timestamp: t1 }),
      createMockCommit({ timestamp: t2 }),
      createMockCommit({ timestamp: t10 }),
    ];

    const rhythm = computeRhythm(commits, 90, fixedNow);
    expect(rhythm.currentStreak).toBe(3);
    expect(rhythm.longestStreak).toBe(3);
    expect(rhythm.activeDaysInWindow).toBe(4);
    expect(rhythm.longestInactiveGap).toBe(7);
  });
});

describe("computePunchCard", () => {
  it("computes after hours percentage correctly", () => {
    // 1 commit on Sunday noon (weekend => after hours)
    // 1 commit on Monday 11pm (weekday night => after hours)
    // 1 commit on Monday 2pm (weekday work hour => standard)
    const sundayNoon = new Date(2026, 7, 30, 12, 0, 0).getTime() / 1000;
    const mondayNight = new Date(2026, 7, 31, 23, 0, 0).getTime() / 1000;
    const mondayAfternoon = new Date(2026, 7, 31, 14, 0, 0).getTime() / 1000;

    const commits = [
      createMockCommit({ timestamp: sundayNoon }),
      createMockCommit({ timestamp: mondayNight }),
      createMockCommit({ timestamp: mondayAfternoon }),
    ];

    const punch = computePunchCard(commits);
    expect(punch.totalCommits).toBe(3);
    expect(punch.afterHoursCommits).toBe(2);
    expect(punch.afterHoursPercentage).toBe(67);
    expect(punch.cells.length).toBe(168);
  });
});

describe("computeLineChanges", () => {
  it("aggregates weekly additions and deletions", () => {
    const fixedNow = new Date(2026, 8, 2, 12, 0, 0).getTime();
    const commits = [
      createMockCommit({
        timestamp: Math.floor(fixedNow / 1000),
        additions: 100,
        deletions: 25,
      }),
    ];

    const buckets = computeLineChanges(commits, 4, fixedNow);
    expect(buckets.length).toBe(4);
    const latest = buckets[buckets.length - 1];
    expect(latest.additions).toBe(100);
    expect(latest.deletions).toBe(25);
    expect(latest.net).toBe(75);
  });
});

describe("computeLocTrend", () => {
  it("reconstructs historical LOC trend from current LOC", () => {
    const day1 = new Date(2026, 7, 10, 10, 0, 0).getTime() / 1000;
    const day2 = new Date(2026, 7, 11, 10, 0, 0).getTime() / 1000;

    const commits = [
      createMockCommit({ timestamp: day1, additions: 50, deletions: 10 }), // net +40
      createMockCommit({ timestamp: day2, additions: 20, deletions: 10 }), // net +10
    ];

    // Current LOC is 1000
    // Total net was +50, so starting LOC was 950.
    // Day 1: 950 + 40 = 990
    // Day 2: 990 + 10 = 1000
    const trend = computeLocTrend(1000, commits);
    expect(trend.length).toBe(2);
    expect(trend[0].totalLoc).toBe(990);
    expect(trend[1].totalLoc).toBe(1000);
  });
});

describe("computeHygiene", () => {
  it("calculates percentages and median churn", () => {
    const commits = [
      createMockCommit({
        summary: "feat: add feature\n\nCo-authored-by: Bob <bob@example.com>",
        gpg_status: "G",
        additions: 10,
        deletions: 0,
        is_merge: false,
      }),
      createMockCommit({
        summary: "Merge branch 'main'",
        gpg_status: "N",
        additions: 0,
        deletions: 0,
        is_merge: true,
      }),
      createMockCommit({
        summary: "revert: fix bug",
        gpg_status: "U",
        additions: 5,
        deletions: 5,
        is_revert: true,
      }),
    ];

    const hygiene = computeHygiene(commits);
    expect(hygiene.totalCommits).toBe(3);
    expect(hygiene.conventionalCount).toBe(2); // "feat:" and "revert:"
    expect(hygiene.signedCount).toBe(2); // "G" and "U"
    expect(hygiene.signedPercentage).toBe(67);
    expect(hygiene.mergeCount).toBe(1);
    expect(hygiene.mergePercentage).toBe(33);
    expect(hygiene.revertCount).toBe(1);
    expect(hygiene.coAuthorCount).toBe(1);
    expect(hygiene.medianChurn).toBe(10);
  });
});

describe("computeHotspotRisks", () => {
  it("joins churn with coverage report to calculate risk items", () => {
    const topFiles = [
      { path: "src/risky.ts", additions: 500, deletions: 200, commits_count: 15 },
      { path: "src/tested.ts", additions: 400, deletions: 100, commits_count: 10 },
      { path: "src/unknown.ts", additions: 50, deletions: 10, commits_count: 2 },
    ];

    const mockCoverage = {
      totals: { lines_found: 100, lines_hit: 50, percentage: 50 },
      files: [
        { path: "src/risky.ts", language: "ts", color_hex: "#3178c6", lines_found: 100, lines_hit: 20, percentage: 20 },
        { path: "src/tested.ts", language: "ts", color_hex: "#3178c6", lines_found: 100, lines_hit: 95, percentage: 95 },
      ],
      artifacts: [],
      truncated: false,
      languages: [],
      families: [],
    };

    const risks = computeHotspotRisks(topFiles, mockCoverage);
    expect(risks.length).toBe(3);
    // risky.ts has 700 churn and 20% coverage -> riskScore = 700 * 0.8 = 560
    expect(risks[0].path).toBe("src/risky.ts");
    expect(risks[0].riskLevel).toBe("critical");
    expect(risks[0].uncoveredLines).toBe(80);

    // tested.ts has 500 churn and 95% coverage -> riskScore = 500 * 0.05 = 25
    expect(risks[2].path).toBe("src/tested.ts");
    expect(risks[2].riskLevel).toBe("low");
  });
});

describe("computePeriodCompare", () => {
  it("calculates deltas between current 30d and prior 30d", () => {
    const nowMs = 1700000000 * 1000;
    const dayMs = 86400 * 1000;

    const commits = [
      // Current period: 10d ago
      createMockCommit({ timestamp: 1700000000 - 10 * 86400, additions: 100, deletions: 20 }),
      createMockCommit({ timestamp: 1700000000 - 5 * 86400, additions: 50, deletions: 10 }),
      // Prior period: 40d ago
      createMockCommit({ timestamp: 1700000000 - 40 * 86400, additions: 50, deletions: 50 }),
    ];

    const deltas = computePeriodCompare(commits, nowMs, 30);
    expect(deltas.currentCommits).toBe(2);
    expect(deltas.priorCommits).toBe(1);
    expect(deltas.commitsDeltaPct).toBe(100); // 1 -> 2 (+100%)
    expect(deltas.currentAdds).toBe(150);
    expect(deltas.priorAdds).toBe(50);
    expect(deltas.addsDeltaPct).toBe(200); // 50 -> 150 (+200%)
  });
});

describe("generatePulseSvgCard", () => {
  it("produces valid standalone SVG containing repo metrics", () => {
    const svg = generatePulseSvgCard({
      repoName: "my-cool-project",
      totalCommits: 142,
      totalLoc: 8520,
      activeDays: 35,
      busFactor: 3,
      halfLifeDays: 95,
      conventionalPct: 88,
      signedPct: 75,
      generatedDate: "2026-09-02",
    });

    expect(svg.startsWith("<svg")).toBe(true);
    expect(svg.endsWith("</svg>")).toBe(true);
    expect(svg).toContain("my-cool-project");
    expect(svg).toContain("8,520");
    expect(svg).toContain("35 active days");
    expect(svg).toContain("Bus Factor");
  });
});

