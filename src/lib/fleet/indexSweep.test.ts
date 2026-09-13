import { describe, expect, it, vi } from "vitest";
import {
  DEVMAP_MISSING,
  buildWasUnchanged,
  countByStatus,
  firstIndexFailure,
  isCleanIndexSweep,
  runIndexSweep,
  summarizeIndexSweep,
  type IndexSweepReport,
  type IndexTarget,
} from "./indexSweep";
import type { DevmapBuildOutcome, InitReport } from "../codeintel/types";

function targets(...names: string[]): IndexTarget[] {
  return names.map((name) => ({ path: `/w/${name}`, label: name }));
}

function init(overrides: Partial<InitReport> = {}): InitReport {
  return {
    repo: "/w/a",
    state_dir: "/w/a/.devmap",
    exclude: { status: "added", file: "/w/a/.git/info/exclude", pattern: "/.devmap/" },
    workspace_registry: "/w/a/.devmap/workspace.json",
    workspace_reason: null,
    devmap_available: true,
    ...overrides,
  };
}

function built(overrides: Partial<DevmapBuildOutcome> = {}): DevmapBuildOutcome {
  return {
    ok: true,
    binary: "/usr/local/bin/devmap",
    lookup: "path_search",
    exit_code: 0,
    stdout: "",
    stderr: "",
    timed_out: false,
    report: {},
    ...overrides,
  };
}

function report(outcomes: IndexSweepReport["outcomes"], total: number, aborted = false) {
  return { outcomes, total, aborted };
}

describe("runIndexSweep", () => {
  it("indexes every open repository and reports each one", async () => {
    const build = vi.fn(async () => built());
    const result = await runIndexSweep(targets("a", "b", "c"), {
      initialize: async () => init(),
      build,
    });
    expect(build).toHaveBeenCalledTimes(3);
    expect(result.outcomes.map((o) => o.status)).toEqual(["indexed", "indexed", "indexed"]);
    expect(isCleanIndexSweep(result)).toBe(true);
  });

  it("passes the whole target set as the open-repository list", async () => {
    // The registry each repository hosts is what makes cross-repository search
    // span the fleet; initializing each one against only itself would leave
    // every registry a single-entry list.
    const initialize = vi.fn(async () => init());
    await runIndexSweep(targets("a", "b"), { initialize, build: async () => built() });
    expect(initialize).toHaveBeenCalledWith("/w/a", ["/w/a", "/w/b"]);
    expect(initialize).toHaveBeenCalledWith("/w/b", ["/w/a", "/w/b"]);
  });

  it("runs one repository at a time", async () => {
    // `devmap build` saturates its cores and takes a per-repository writer lock
    // that the serve daemon also contends for, so overlapping builds would
    // queue on the lock rather than finish sooner.
    let active = 0;
    let peak = 0;
    const build = vi.fn(async () => {
      active += 1;
      peak = Math.max(peak, active);
      await Promise.resolve();
      active -= 1;
      return built();
    });
    await runIndexSweep(targets("a", "b", "c"), { initialize: async () => init(), build });
    expect(peak).toBe(1);
  });

  it("keeps 'already current' apart from 'indexed'", async () => {
    const result = await runIndexSweep(targets("a", "b"), {
      initialize: async () => init(),
      build: async (repoPath) =>
        repoPath === "/w/a" ? built({ report: { unchanged: true } }) : built(),
    });
    expect(countByStatus(result)).toMatchObject({ current: 1, indexed: 1 });
    // A fleet that needed nothing must not read like one that rebuilt.
    expect(summarizeIndexSweep(result)).toContain("already current");
  });

  it("records a failed build with the kernel's own reason", async () => {
    const result = await runIndexSweep(targets("a"), {
      initialize: async () => init(),
      build: async () => built({ ok: false, stderr: "store is locked by pid 41" }),
    });
    expect(result.outcomes[0].status).toBe("failed");
    expect(result.outcomes[0].reason).toBe("store is locked by pid 41");
    expect(isCleanIndexSweep(result)).toBe(false);
    expect(firstIndexFailure(result)?.label).toBe("a");
  });

  it("names a deadline rather than reporting an empty failure", async () => {
    const result = await runIndexSweep(targets("a"), {
      initialize: async () => init(),
      build: async () => built({ ok: false, timed_out: true, stderr: "" }),
    });
    expect(result.outcomes[0].reason).toMatch(/deadline/);
  });

  it("turns a thrown initialization into a failure for that repository only", async () => {
    const result = await runIndexSweep(targets("a", "b"), {
      initialize: async (repoPath) => {
        if (repoPath === "/w/a") throw new Error("REPOSITORY_TRUST_REQUIRED");
        return init();
      },
      build: async () => built(),
    });
    expect(result.outcomes[0]).toMatchObject({ status: "failed", reason: "REPOSITORY_TRUST_REQUIRED" });
    expect(result.outcomes[1].status).toBe("indexed");
  });

  it("stops when devmap is missing and says so for every repository it did not run", async () => {
    // The honesty case: a sweep that could not do anything must not render the
    // same as a fleet that needed nothing doing.
    const build = vi.fn(async () => built());
    const result = await runIndexSweep(targets("a", "b", "c"), {
      initialize: async () => init({ devmap_available: false, workspace_registry: null }),
      build,
    });
    expect(build).not.toHaveBeenCalled();
    expect(result.outcomes).toHaveLength(3);
    expect(result.outcomes.every((o) => o.status === "skipped")).toBe(true);
    expect(result.outcomes[2].reason).toBe(DEVMAP_MISSING);
    expect(isCleanIndexSweep(result)).toBe(false);
    expect(summarizeIndexSweep(result)).toContain("devmap is not installed");
    expect(summarizeIndexSweep(result)).not.toMatch(/^Indexed 0 of/);
  });

  it("stops between repositories when the user aborts", async () => {
    const signal = { aborted: false };
    const build = vi.fn(async () => {
      signal.aborted = true;
      return built();
    });
    const result = await runIndexSweep(targets("a", "b", "c"), {
      initialize: async () => init(),
      build,
      signal,
    });
    expect(build).toHaveBeenCalledTimes(1);
    expect(result.aborted).toBe(true);
    expect(result.outcomes).toHaveLength(1);
    expect(result.total).toBe(3);
  });

  it("reports progress against the real total, not against what it reached", async () => {
    const seen: string[] = [];
    const progress: [number, number][] = [];
    await runIndexSweep(targets("a", "b"), {
      initialize: async () => init(),
      build: async () => built(),
      onStart: (target) => seen.push(target.label),
      onProgress: (done, total) => progress.push([done, total]),
    });
    expect(seen).toEqual(["a", "b"]);
    expect(progress).toEqual([
      [1, 2],
      [2, 2],
    ]);
  });

  it("does nothing, loudly, with no open repositories", async () => {
    const result = await runIndexSweep([], { initialize: async () => init(), build: async () => built() });
    expect(result.outcomes).toEqual([]);
    expect(summarizeIndexSweep(result)).toBe("No open repositories to index.");
  });
});

describe("buildWasUnchanged", () => {
  it("reads the kernel's own flag", () => {
    expect(buildWasUnchanged(built({ report: { unchanged: true } }))).toBe(true);
    expect(buildWasUnchanged(built({ report: { unchanged: false } }))).toBe(false);
  });

  it("treats an unreadable report as work done rather than as already current", () => {
    // Erring the other way would claim a repository was current on the strength
    // of a payload we could not read.
    for (const report of [undefined, null, "unchanged", 1, {}]) {
      expect(buildWasUnchanged(built({ report })), String(report)).toBe(false);
    }
  });
});

describe("summarizeIndexSweep", () => {
  const ok = (label: string, status: "indexed" | "current" | "failed" = "indexed") => ({
    path: `/w/${label}`,
    label,
    status,
    reason: status === "failed" ? "boom" : null,
  });

  it("does not claim the whole fleet when only some of it was done", () => {
    const line = summarizeIndexSweep(report([ok("a"), ok("b", "failed")], 2));
    expect(line).toContain("1 of 2 repositories");
    expect(line).toContain("1 failed");
  });

  it("says plainly when everything was indexed", () => {
    expect(summarizeIndexSweep(report([ok("a"), ok("b")], 2))).toBe("Indexed 2 repositories.");
  });

  it("uses the singular for one repository", () => {
    expect(summarizeIndexSweep(report([ok("a")], 1))).toBe("Indexed 1 repository.");
  });

  it("carries both numbers when the user stopped it", () => {
    const line = summarizeIndexSweep(report([ok("a")], 5, true));
    expect(line).toContain("1 of 5 repositories");
    expect(line).toMatch(/Stopped/);
  });

  it("never reports an aborted sweep as clean", () => {
    expect(isCleanIndexSweep(report([ok("a")], 5, true))).toBe(false);
  });

  it("never reports a short sweep as clean even when nothing failed", () => {
    // Fewer outcomes than targets means repositories were never reached.
    expect(isCleanIndexSweep(report([ok("a")], 3))).toBe(false);
  });
});
