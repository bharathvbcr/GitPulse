import { describe, expect, it } from "vitest";
import {
  DEFAULT_BULK_CONCURRENCY,
  MAX_BULK_TARGETS,
  firstFailure,
  isCleanSweep,
  runAcrossRepos,
  summarizeRun,
  type RepoTarget,
} from "./workspaceOps";

function targets(count: number): RepoTarget[] {
  return Array.from({ length: count }, (_, i) => ({
    path: `/r/repo-${i}`,
    label: `repo-${i}`,
  }));
}

/** A clock the tests advance by hand, so durations are deterministic. */
function fakeClock() {
  let t = 0;
  return {
    now: () => t,
    advance: (ms: number) => {
      t += ms;
    },
  };
}

describe("runAcrossRepos", () => {
  it("visits every repository and reports each one", async () => {
    const seen: string[] = [];
    const report = await runAcrossRepos(targets(5), async (target) => {
      seen.push(target.path);
    });
    expect(seen).toHaveLength(5);
    expect(report.succeeded).toBe(5);
    expect(report.failed).toBe(0);
    expect(report.skipped).toBe(0);
    expect(isCleanSweep(report)).toBe(true);
  });

  it("returns results in target order, not completion order", async () => {
    // A report that reordered itself by whoever finished first would be
    // unstable between runs and impossible to diff.
    const report = await runAcrossRepos(
      targets(4),
      async (target) => {
        const delay = target.path.endsWith("0") ? 20 : 0;
        await new Promise((resolve) => setTimeout(resolve, delay));
      },
      { concurrency: 4 },
    );
    expect(report.results.map((r) => r.label)).toEqual([
      "repo-0",
      "repo-1",
      "repo-2",
      "repo-3",
    ]);
  });

  it("keeps going when one repository throws", async () => {
    // Promise.all would abandon the other 23 repositories here.
    const report = await runAcrossRepos(targets(4), async (target) => {
      if (target.label === "repo-1") throw new Error("remote unreachable");
    });
    expect(report.succeeded).toBe(3);
    expect(report.failed).toBe(1);
    const failure = firstFailure(report);
    expect(failure?.label).toBe("repo-1");
    expect(failure?.error).toBe("remote unreachable");
  });

  it("never rejects, whatever the task throws", async () => {
    const report = await runAcrossRepos(targets(3), async () => {
      // eslint-disable-next-line no-throw-literal
      throw "a bare string";
    });
    expect(report.failed).toBe(3);
    expect(report.results[0].error).toBe("a bare string");
  });

  it("keeps skipped separate from succeeded", async () => {
    // The property the whole module exists for: a repository that was not
    // fetched must never be counted as fetched.
    const report = await runAcrossRepos(targets(4), async (target) =>
      target.label === "repo-2" ? { skip: "a merge is in progress here" } : undefined,
    );
    expect(report.succeeded).toBe(3);
    expect(report.skipped).toBe(1);
    expect(report.failed).toBe(0);
    const skipped = report.results.find((r) => r.status === "skipped");
    expect(skipped?.reason).toBe("a merge is in progress here");
    expect(skipped?.error).toBeUndefined();
    expect(isCleanSweep(report)).toBe(false);
  });

  it("honours the concurrency cap", async () => {
    let inFlight = 0;
    let peak = 0;
    await runAcrossRepos(
      targets(12),
      async () => {
        inFlight += 1;
        peak = Math.max(peak, inFlight);
        await new Promise((resolve) => setTimeout(resolve, 1));
        inFlight -= 1;
      },
      { concurrency: 3 },
    );
    expect(peak).toBeLessThanOrEqual(3);
    expect(peak).toBeGreaterThan(1);
  });

  it("defaults to a bounded concurrency rather than everything at once", async () => {
    let peak = 0;
    let inFlight = 0;
    await runAcrossRepos(targets(20), async () => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 1));
      inFlight -= 1;
    });
    expect(peak).toBeLessThanOrEqual(DEFAULT_BULK_CONCURRENCY);
  });

  it("clamps a nonsensical concurrency instead of stalling or exploding", async () => {
    for (const concurrency of [0, -5, Number.NaN]) {
      const report = await runAcrossRepos(targets(3), async () => {}, { concurrency });
      expect(report.succeeded, `concurrency ${concurrency}`).toBe(3);
    }
  });

  it("stops starting work once cancelled and marks the rest skipped", async () => {
    const signal = { aborted: false };
    let started = 0;
    const report = await runAcrossRepos(
      targets(10),
      async () => {
        started += 1;
        if (started === 2) signal.aborted = true;
      },
      { concurrency: 1, signal },
    );
    expect(report.cancelled).toBe(true);
    expect(report.succeeded).toBe(2);
    expect(report.skipped).toBe(8);
    // A cancelled repository is skipped, never failed — nothing went wrong
    // with it, it simply was not reached.
    expect(report.results[9].status).toBe("skipped");
    expect(report.results[9].reason).toContain("Cancelled");
  });

  it("reports an already-cancelled run without running anything", async () => {
    let ran = 0;
    const report = await runAcrossRepos(
      targets(4),
      async () => {
        ran += 1;
      },
      { signal: { aborted: true } },
    );
    expect(ran).toBe(0);
    expect(report.skipped).toBe(4);
    expect(report.cancelled).toBe(true);
  });

  it("deduplicates repeated paths so two tabs cannot contend for one lock", async () => {
    const ran: string[] = [];
    const report = await runAcrossRepos(
      [
        { path: "/r/a", label: "a" },
        { path: "/r/b", label: "b" },
        { path: "/r/a", label: "a (again)" },
      ],
      async (target) => {
        ran.push(target.path);
      },
    );
    expect(ran).toEqual(["/r/a", "/r/b"]);
    expect(report.results).toHaveLength(2);
  });

  it("bounds how many repositories one run may visit", async () => {
    const report = await runAcrossRepos(targets(MAX_BULK_TARGETS + 20), async () => {});
    expect(report.results).toHaveLength(MAX_BULK_TARGETS);
  });

  it("handles an empty workspace without inventing a result", async () => {
    const report = await runAcrossRepos([], async () => {});
    expect(report.results).toEqual([]);
    expect(isCleanSweep(report)).toBe(false);
    expect(summarizeRun(report, "Fetched")).toBe("Nothing to Fetched.");
  });

  it("reports progress once per repository, in settle order", async () => {
    const seen: number[] = [];
    await runAcrossRepos(
      targets(5),
      async () => {},
      {
        concurrency: 1,
        onProgress: (done, total) => {
          seen.push(done);
          expect(total).toBe(5);
        },
      },
    );
    expect(seen).toEqual([1, 2, 3, 4, 5]);
  });

  it("records durations from the injected clock", async () => {
    const clock = fakeClock();
    const report = await runAcrossRepos(
      targets(1),
      async () => {
        clock.advance(250);
      },
      { now: clock.now, concurrency: 1 },
    );
    expect(report.results[0].durationMs).toBe(250);
    // A skipped repository has no duration to report.
    const skipReport = await runAcrossRepos(
      targets(1),
      async () => ({ skip: "nope" }),
      { now: clock.now },
    );
    expect(skipReport.results[0].durationMs).toBe(0);
  });
});

describe("summarizeRun", () => {
  const report = (succeeded: number, failed: number, skipped: number, cancelled = false) => ({
    results: [
      ...Array.from({ length: succeeded }, (_, i) => ({
        path: `/ok${i}`,
        label: `ok${i}`,
        status: "ok" as const,
        durationMs: 1,
      })),
      ...Array.from({ length: failed }, (_, i) => ({
        path: `/bad${i}`,
        label: `bad${i}`,
        status: "failed" as const,
        error: "boom",
        durationMs: 1,
      })),
      ...Array.from({ length: skipped }, (_, i) => ({
        path: `/skip${i}`,
        label: `skip${i}`,
        status: "skipped" as const,
        reason: "parked",
        durationMs: 0,
      })),
    ],
    succeeded,
    failed,
    skipped,
    cancelled,
    totalMs: 10,
  });

  it("states the plain total when everything succeeded", () => {
    expect(summarizeRun(report(24, 0, 0), "Fetched")).toBe("Fetched 24 of 24");
  });

  it("never lets a partial sweep read as a whole one", () => {
    // "Fetched 20 of 24" alone reads as rounding; the breakdown cannot be
    // misread as full coverage.
    const line = summarizeRun(report(20, 1, 3), "Fetched");
    expect(line).toBe("Fetched 20 of 24 — 1 failed, 3 skipped");
  });

  it("names skipped repositories even when nothing failed", () => {
    expect(summarizeRun(report(21, 0, 3), "Pulled")).toContain("3 skipped");
  });

  it("marks a cancelled run as cancelled", () => {
    expect(summarizeRun(report(2, 0, 8, true), "Fetched")).toContain("(cancelled)");
  });

  it("says there was nothing to do rather than claiming success", () => {
    expect(summarizeRun(report(0, 0, 0), "Fetched")).toBe("Nothing to Fetched.");
  });
});

describe("isCleanSweep", () => {
  it("is false when anything was skipped, not just when something failed", () => {
    const base = { results: [], succeeded: 0, failed: 0, skipped: 0, cancelled: false, totalMs: 0 };
    const one = { path: "/a", label: "a", status: "ok" as const, durationMs: 1 };
    expect(isCleanSweep({ ...base, results: [one], succeeded: 1 })).toBe(true);
    expect(isCleanSweep({ ...base, results: [one], succeeded: 1, skipped: 1 })).toBe(false);
    expect(isCleanSweep({ ...base, results: [one], succeeded: 1, failed: 1 })).toBe(false);
    expect(isCleanSweep(base)).toBe(false);
  });
});
