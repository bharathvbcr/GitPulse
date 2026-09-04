import { describe, expect, it } from "vitest";
import { unknownFacts, type RepoFacts } from "../repos/facts";
import { WATCH_ACTIVE, watchFailed } from "../repos/watchState";
import { buildFleetRows, parseStamp, placeholderRow, type FleetRowInputs } from "./row";
import type { FleetMetrics, FleetRepoFacet, FleetSnapshot } from "./types";

const NOW = Date.parse("2026-09-04T12:00:00Z");

function facts(path: string, overrides: Partial<RepoFacts> = {}): RepoFacts {
  return {
    ...unknownFacts(path, path.split("/").pop() ?? path),
    hydrated: true,
    watch: WATCH_ACTIVE,
    branch: "main",
    ...overrides,
  };
}

function metrics(overrides: Partial<FleetMetrics> = {}): FleetMetrics {
  return {
    repo_path: "/repo/a",
    loc: null,
    loc_language: null,
    loc_truncated: false,
    loc_at: null,
    storage_bytes: null,
    storage_git_bytes: null,
    storage_reclaimable_bytes: null,
    storage_truncated: false,
    storage_at: null,
    vulns_critical: null,
    vulns_high: null,
    vulns_moderate: null,
    vulns_low: null,
    vulns_unknown: null,
    vulns_total: null,
    health_complete: false,
    health_at: null,
    coverage_pct: null,
    coverage_truncated: false,
    coverage_at: null,
    ...overrides,
  };
}

function facet(path: string, overrides: Partial<FleetRepoFacet> = {}): FleetRepoFacet {
  return {
    repo_path: path,
    ok: true,
    error: "",
    worktrees_ok: true,
    worktrees_error: "",
    worktrees: 1,
    agents: { sessions: 0, kinds: [] },
    last_commit_ok: true,
    last_commit_epoch: 1_757_000_000,
    metrics_ok: true,
    metrics_error: "",
    metrics: null,
    ...overrides,
  };
}

function snapshot(facets: FleetRepoFacet[]): FleetSnapshot {
  return {
    repos: facets,
    requested: facets.length,
    scanned: facets.length,
    truncated: false,
    duration_ms: 12,
  };
}

function inputs(overrides: Partial<FleetRowInputs> = {}): FleetRowInputs {
  return {
    open: [],
    recents: [],
    snapshot: null,
    snapshotError: null,
    scanFailures: {},
    now: NOW,
    ...overrides,
  };
}

describe("parseStamp", () => {
  it("reads an ISO stamp the backend wrote", () => {
    expect(parseStamp("2026-09-04T11:00:00Z")).toBe(Date.parse("2026-09-04T11:00:00Z"));
  });

  it("returns null rather than NaN for junk, which would render as an age", () => {
    expect(parseStamp("not a date")).toBeNull();
    expect(parseStamp("")).toBeNull();
    expect(parseStamp(null)).toBeNull();
    expect(parseStamp(undefined)).toBeNull();
  });
});

describe("Tier 0 cells", () => {
  it("reads changes and sync from a hydrated session", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [
          facts("/repo/a", {
            changedFiles: 5,
            stagedFiles: 2,
            conflictedFiles: 0,
            additions: 120,
            deletions: 30,
            unpushedCommits: 3,
            behindCommits: 1,
            stashEntries: 2,
          }),
        ],
      }),
    );
    expect(row.changes).toEqual({
      kind: "read",
      value: { files: 5, staged: 2, conflicted: 0, additions: 120, deletions: 30 },
      at: null,
      partial: false,
    });
    expect(row.sync).toMatchObject({ kind: "read", value: { ahead: 3, behind: 1, stash: 2 } });
  });

  it("marks an unhydrated repository unscanned, never clean", () => {
    const [row] = buildFleetRows(inputs({ open: [facts("/repo/a", { hydrated: false })] }));
    expect(row.changes.kind).toBe("unscanned");
    expect(row.sync.kind).toBe("unscanned");
    // The severity has to say so too, or the row reads as a healthy repo with
    // nothing in it.
    expect(row.severity).toBe("unknown");
  });

  it("reports a failed snapshot as failed, carrying the reason", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a", { loadFailed: true, loadError: "not a repository" })] }),
    );
    expect(row.changes).toEqual({ kind: "failed", reason: "not a repository" });
    expect(row.severity).toBe("unknown");
  });

  it("flags churn as partial when a status row could not be parsed", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a", { changedFiles: 1, churnPartial: true })] }),
    );
    expect(row.changes).toMatchObject({ kind: "read", partial: true });
  });

  it("flags sync as partial when the stash probe failed", () => {
    const [row] = buildFleetRows(inputs({ open: [facts("/repo/a", { stashFailed: true })] }));
    expect(row.sync).toMatchObject({ kind: "read", partial: true });
  });

  it("takes its severity and headline from the shared risk model", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a", { changedFiles: 3, conflictedFiles: 3 })] }),
    );
    expect(row.severity).toBe("conflicts");
    expect(row.headline).toBe("3 files with conflicts");
  });

  it("says clean when nothing is at risk", () => {
    const [row] = buildFleetRows(inputs({ open: [facts("/repo/a")] }));
    expect(row.severity).toBe("clean");
    expect(row.headline).toBe("clean");
  });
});

describe("watch warnings", () => {
  it("stays silent for a confirmed live watch", () => {
    const [row] = buildFleetRows(inputs({ open: [facts("/repo/a")] }));
    expect(row.watchWarning).toBeNull();
  });

  it("names the reason a watch is degraded", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a", { watch: watchFailed("watch table is full") })] }),
    );
    expect(row.watchWarning).toContain("watch table is full");
  });

  it("does not warn before the first snapshot has landed", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a", { hydrated: false, watch: { status: "unknown", reason: null } })] }),
    );
    expect(row.watchWarning).toBeNull();
  });
});

describe("Tier 1 cells", () => {
  it("is unscanned before the sweep has run", () => {
    const [row] = buildFleetRows(inputs({ open: [facts("/repo/a")] }));
    expect(row.work.kind).toBe("unscanned");
    expect(row.activity.kind).toBe("unscanned");
  });

  it("reads worktrees, agents and last activity from the facet", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", {
            worktrees: 4,
            agents: { sessions: 3, kinds: [{ kind: "claude", sessions: 3 }] },
            last_commit_epoch: 1_757_000_000,
          }),
        ]),
      }),
    );
    expect(row.work).toMatchObject({
      kind: "read",
      value: { worktrees: 4, agentSessions: 3, agentKinds: ["claude"] },
    });
    expect(row.activity).toMatchObject({ kind: "read", value: 1_757_000_000 * 1000 });
  });

  it("fails one repository's Tier 1 cells without touching the others", () => {
    const rows = buildFleetRows(
      inputs({
        open: [facts("/repo/a"), facts("/repo/b")],
        snapshot: snapshot([
          facet("/repo/a", { ok: false, error: "permission denied" }),
          facet("/repo/b"),
        ]),
      }),
    );
    expect(rows[0].work).toEqual({ kind: "failed", reason: "permission denied" });
    expect(rows[1].work.kind).toBe("read");
  });

  it("fails only the work cell when the worktree list alone is unreadable", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", { worktrees_ok: false, worktrees_error: "git worktree failed" }),
        ]),
      }),
    );
    expect(row.work).toEqual({ kind: "failed", reason: "git worktree failed" });
    expect(row.activity.kind).toBe("read");
  });

  it("distinguishes a repository with no commits from one that could not be read", () => {
    const rows = buildFleetRows(
      inputs({
        open: [facts("/repo/a"), facts("/repo/b")],
        snapshot: snapshot([
          facet("/repo/a", { last_commit_ok: true, last_commit_epoch: 0 }),
          facet("/repo/b", { last_commit_ok: false, last_commit_epoch: 0 }),
        ]),
      }),
    );
    expect(rows[0].activity).toMatchObject({ kind: "read", value: 0 });
    expect(rows[1].activity.kind).toBe("failed");
  });

  it("fails every Tier 1 cell when the sweep itself failed", () => {
    const rows = buildFleetRows(
      inputs({ open: [facts("/repo/a"), facts("/repo/b")], snapshotError: "backend unavailable" }),
    );
    for (const row of rows) {
      expect(row.work).toEqual({ kind: "failed", reason: "backend unavailable" });
      expect(row.activity).toEqual({ kind: "failed", reason: "backend unavailable" });
    }
  });
});

describe("Tier 2 cells", () => {
  it("is unscanned when the repository has no ledger metrics at all", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a")], snapshot: snapshot([facet("/repo/a")]) }),
    );
    for (const cell of [row.loc, row.storage, row.health, row.coverage]) {
      expect(cell.kind).toBe("unscanned");
    }
  });

  it("is unscanned for a family whose timestamp is missing, even with a value", () => {
    // A value with no timestamp is a value of unknown age. Rendering it as a
    // measurement would put a number on screen nobody can date.
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([facet("/repo/a", { metrics: metrics({ loc: 4200, loc_at: null }) })]),
      }),
    );
    expect(row.loc.kind).toBe("unscanned");
  });

  it("reads each family with its own age", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", {
            metrics: metrics({
              loc: 4200,
              loc_language: "Rust",
              loc_at: "2026-09-04T11:00:00Z",
              storage_bytes: 1024,
              storage_git_bytes: 512,
              storage_reclaimable_bytes: 256,
              storage_at: "2026-09-03T11:00:00Z",
              vulns_total: 2,
              vulns_high: 2,
              health_complete: true,
              health_at: "2026-09-02T11:00:00Z",
              coverage_pct: 81.5,
              coverage_at: "2026-09-01T11:00:00Z",
            }),
          }),
        ]),
      }),
    );
    expect(row.loc).toMatchObject({ kind: "read", value: { lines: 4200, language: "Rust" } });
    expect(row.storage).toMatchObject({
      kind: "read",
      value: { bytes: 1024, gitBytes: 512, reclaimableBytes: 256 },
    });
    expect(row.health).toMatchObject({ kind: "read", value: { total: 2, high: 2, complete: true } });
    expect(row.coverage).toMatchObject({ kind: "read", value: 81.5 });
    // Ages are per family, not one timestamp for the row.
    expect(row.loc.kind === "read" && row.loc.at).toBe(Date.parse("2026-09-04T11:00:00Z"));
    expect(row.coverage.kind === "read" && row.coverage.at).toBe(Date.parse("2026-09-01T11:00:00Z"));
  });

  it("marks an incomplete audit partial so zero findings cannot read as clean", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", {
            metrics: metrics({
              vulns_total: 0,
              health_complete: false,
              health_at: "2026-09-02T11:00:00Z",
            }),
          }),
        ]),
      }),
    );
    expect(row.health).toMatchObject({ kind: "read", partial: true });
  });

  it("marks a truncated storage walk partial", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", {
            metrics: metrics({
              storage_bytes: 99,
              storage_truncated: true,
              storage_at: "2026-09-02T11:00:00Z",
            }),
          }),
        ]),
      }),
    );
    expect(row.storage).toMatchObject({ kind: "read", partial: true });
  });

  it("lets a fresh failure beat a cached value, keeping the value's age in the reason", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", {
            metrics: metrics({ storage_bytes: 99, storage_at: "2026-09-01T12:00:00Z" }),
          }),
        ]),
        scanFailures: { "/repo/a": { storage: "scan timed out" } },
      }),
    );
    expect(row.storage.kind).toBe("failed");
    expect(row.storage.kind === "failed" && row.storage.reason).toBe(
      "scan timed out — last successful scan 3d ago",
    );
  });

  it("reports a failure with no cached value plainly", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a")], scanFailures: { "/repo/a": { health: "npm not found" } } }),
    );
    expect(row.health).toEqual({ kind: "failed", reason: "npm not found" });
  });

  it("never leaves a failure reason empty", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a")], scanFailures: { "/repo/a": { loc: "   " } } }),
    );
    expect(row.loc.kind === "failed" && row.loc.reason.length).toBeGreaterThan(0);
  });
});

describe("a row is never called clean over a check that could not run", () => {
  it("downgrades clean to unknown when the cheap sweep could not read the repository", () => {
    // Found by driving the real grid: a repository whose Tier 1 facet failed
    // still rendered "clean" from its Tier 0 counts, beside four cells reading
    // "could not read". The headline is a claim about the whole repository,
    // so it cannot be made while any check is unaccounted for.
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([facet("/repo/a", { ok: false, error: "not a git repository" })]),
      }),
    );
    expect(row.severity).toBe("unknown");
    expect(row.headline).toContain("not a git repository");
  });

  it("downgrades clean when the sweep as a whole failed", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a")], snapshotError: "backend unavailable" }),
    );
    expect(row.severity).toBe("unknown");
    expect(row.headline).toContain("backend unavailable");
  });

  it("downgrades clean when only the worktree list was unreadable", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", { worktrees_ok: false, worktrees_error: "git worktree failed" }),
        ]),
      }),
    );
    expect(row.severity).toBe("unknown");
  });

  it("never upgrades a worse severity into unknown", () => {
    // `unknown` ranks below conflicts and a parked operation. A failed sweep
    // must not make a repository with real conflicts look merely unreadable.
    const rows = buildFleetRows(
      inputs({
        open: [
          facts("/repo/a", { changedFiles: 2, conflictedFiles: 2 }),
          facts("/repo/b", { changedFiles: 4 }),
        ],
        snapshotError: "backend unavailable",
      }),
    );
    expect(rows[0].severity).toBe("conflicts");
    expect(rows[0].headline).toBe("2 files with conflicts");
    // `uncommitted` ranks BELOW unknown, so that one does move.
    expect(rows[1].severity).toBe("unknown");
  });

  it("leaves a fully-read clean repository clean", () => {
    const [row] = buildFleetRows(
      inputs({ open: [facts("/repo/a")], snapshot: snapshot([facet("/repo/a")]) }),
    );
    expect(row.severity).toBe("clean");
    expect(row.headline).toBe("clean");
  });
});

describe("an unreadable ledger", () => {
  it("fails every Tier 2 cell rather than calling them unscanned", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([
          facet("/repo/a", { metrics_ok: false, metrics_error: "database is locked" }),
        ]),
      }),
    );
    // "We could not read this repository's scan history" and "nobody has
    // scanned this repository" are different facts, and only one of them is
    // a reason to go press Scan.
    for (const cell of [row.loc, row.storage, row.health, row.coverage]) {
      expect(cell).toEqual({ kind: "failed", reason: "database is locked" });
    }
  });

  it("lets this session's own scan failure win over the ledger's", () => {
    const [row] = buildFleetRows(
      inputs({
        open: [facts("/repo/a")],
        snapshot: snapshot([facet("/repo/a", { metrics_ok: false, metrics_error: "locked" })]),
        scanFailures: { "/repo/a": { health: "npm audit exited 1" } },
      }),
    );
    expect(row.health).toEqual({ kind: "failed", reason: "npm audit exited 1" });
    expect(row.storage).toEqual({ kind: "failed", reason: "locked" });
  });
});

describe("recents rows", () => {
  it("declares every live fact unknown and says why", () => {
    const rows = buildFleetRows(inputs({ recents: [{ path: "/repo/old", label: "old" }] }));
    expect(rows).toHaveLength(1);
    expect(rows[0].presence).toBe("recent");
    expect(rows[0].severity).toBe("unknown");
    expect(rows[0].headline).toContain("not open");
    for (const cell of [rows[0].changes, rows[0].sync, rows[0].work, rows[0].activity]) {
      expect(cell.kind).toBe("unscanned");
    }
  });

  it("still shows what the repository's own ledger recorded", () => {
    const rows = buildFleetRows(
      inputs({
        recents: [{ path: "/repo/old", label: "old" }],
        snapshot: snapshot([
          facet("/repo/old", {
            metrics: metrics({ loc: 900, loc_language: "Go", loc_at: "2026-08-30T12:00:00Z" }),
          }),
        ]),
      }),
    );
    expect(rows[0].loc).toMatchObject({ kind: "read", value: { lines: 900, language: "Go" } });
  });

  it("never renders a path twice when it is both open and recent", () => {
    const rows = buildFleetRows(
      inputs({ open: [facts("/repo/a")], recents: [{ path: "/repo/a", label: "a" }] }),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].presence).toBe("open");
  });

  it("keeps open repositories ahead of recents", () => {
    const rows = buildFleetRows(
      inputs({ open: [facts("/repo/z")], recents: [{ path: "/repo/a", label: "a" }] }),
    );
    expect(rows.map((row) => row.presence)).toEqual(["open", "recent"]);
  });
});

describe("placeholderRow", () => {
  it("is entirely unknown, with nothing reading as measured", () => {
    const row = placeholderRow("/repo/x", "x", NOW);
    expect(row.severity).toBe("unknown");
    for (const cell of [row.changes, row.sync, row.work, row.activity, row.loc, row.storage]) {
      expect(cell.kind).toBe("unscanned");
    }
  });
});
