import { describe, expect, it } from "vitest";
import { collectRepoFacts, pathsTrustedForBackground, toWipInput, unknownFacts, type RepoFacts } from "./facts";
import { IDLE_OPERATION } from "./operation";
import { createSession } from "./projection";
import { WATCH_ACTIVE, WATCH_UNKNOWN } from "./watchState";
import { repoWip } from "./wipSummary";

function facts(overrides: Partial<RepoFacts> = {}): RepoFacts {
  return { ...unknownFacts("/repo/alpha", "alpha"), hydrated: true, ...overrides };
}

describe("unknownFacts", () => {
  it("reports every count as zero but marks itself unhydrated", () => {
    const blank = unknownFacts("/repo/beta", "beta");
    expect(blank.changedFiles).toBe(0);
    expect(blank.unpushedCommits).toBe(0);
    expect(blank.stashEntries).toBe(0);
    // The zeros above are only safe because this flag says they are not
    // measurements. Flipping it would turn "we never looked" into "clean".
    expect(blank.hydrated).toBe(false);
    expect(blank.operation).toEqual(IDLE_OPERATION);
    expect(blank.watch).toEqual(WATCH_UNKNOWN);
  });

  it("carries the identity it was given", () => {
    const blank = unknownFacts("/repo/beta", "beta");
    expect(blank.path).toBe("/repo/beta");
    expect(blank.label).toBe("beta");
  });
});

describe("toWipInput", () => {
  it("passes the risk-bearing counts through unchanged", () => {
    const input = toWipInput(
      facts({
        changedFiles: 7,
        conflictedFiles: 2,
        unpushedCommits: 3,
        stashEntries: 1,
      }),
    );
    expect(input).toMatchObject({
      path: "/repo/alpha",
      label: "alpha",
      changedFiles: 7,
      conflictedFiles: 2,
      unpushedCommits: 3,
      stashEntries: 1,
      hydrated: true,
    });
  });

  it("folds an unreadable stash into loadFailed", () => {
    // An unreadable stash list is work that exists nowhere else, invisible.
    // The risk model has one "could not read" channel and it must land there,
    // or a forgotten stash renders as no stash.
    const input = toWipInput(facts({ stashFailed: true }));
    expect(input.loadFailed).toBe(true);
    expect(repoWip(input).severity).toBe("unknown");
  });

  it("keeps loadFailed true when the snapshot itself failed", () => {
    const input = toWipInput(facts({ loadFailed: true, loadError: "boom" }));
    expect(input.loadFailed).toBe(true);
  });

  it("does not invent loadFailed for a healthy repository", () => {
    const input = toWipInput(facts({ watch: WATCH_ACTIVE }));
    expect(input.loadFailed).toBe(false);
    expect(repoWip(input).reasons).toEqual([]);
  });

  it("drops the fields the risk model does not read", () => {
    const input = toWipInput(facts({ additions: 40, deletions: 9, behindCommits: 5 }));
    expect(Object.keys(input).sort()).toEqual([
      "changedFiles",
      "conflictedFiles",
      "hydrated",
      "label",
      "loadFailed",
      "operation",
      "path",
      "stashEntries",
      "unpushedCommits",
    ]);
  });
});

describe("pathsTrustedForBackground", () => {
  it("excludes untrusted and missing tabs from both scopes and activeKey, and omits inactive parked tabs from liveKeys", () => {
    const result = pathsTrustedForBackground(
      [
        { path: "/repo/trusted" },
        { path: "/repo/untrusted", trustRequired: true },
        { path: "/repo/missing", missing: true },
        { path: "/repo/parked-bg", parked: true },
        { path: "/repo/parked-active", parked: true },
      ],
      "/repo/parked-active",
    );
    expect(result).toEqual({
      activeKey: "/repo/parked-active",
      retainedKeys: ["/repo/trusted", "/repo/parked-bg", "/repo/parked-active"],
      liveKeys: ["/repo/trusted", "/repo/parked-active"],
    });

    const blockedActive = pathsTrustedForBackground(
      [{ path: "/repo/untrusted", trustRequired: true }, { path: "/repo/ok" }],
      "/repo/untrusted",
    );
    expect(blockedActive.activeKey).toBeNull();
  });
});

describe("collectRepoFacts", () => {
  it("extracts churnPartial floors, folds stashTruncated into stashFailed, and defaults unhydrated tabs", () => {
    const partial = createSession({ id: "a", path: "/work/app/core", pinned: false }, {
      hasHydrated: true,
      currentBranch: "feat",
      statuses: [
        { path: "a.ts", status_code: "M", is_staged: true, is_conflicted: false, additions: 3, deletions: 1, warnings: ["numstat skipped"] },
        { path: "b.ts", status_code: "UU", is_staged: false, is_conflicted: true, additions: 2, deletions: 4 },
      ],
      branches: [{
        name: "feat",
        is_remote: false,
        remote_name: null,
        is_current: true,
        is_default: false,
        ahead_count: 2,
        behind_count: 1,
        last_commit_id: "abc",
        tip_commit_id: "abc",
        last_commit_summary: "wip",
        last_commit_time: 1,
      } as never],
      stashTruncated: true,
    });
    const list = collectRepoFacts(
      [
        { id: "a", path: "/work/app/core" },
        { id: "b", path: "/work/lib/core" },
      ],
      { a: partial },
    );
    expect(list[0]).toMatchObject({
      path: "/work/app/core",
      label: "app/core",
      branch: "feat",
      changedFiles: 2,
      conflictedFiles: 1,
      stagedFiles: 1,
      additions: 5,
      deletions: 5,
      churnPartial: true,
      unpushedCommits: 2,
      behindCommits: 1,
      stashFailed: true,
      hydrated: true,
    });
    expect(list[1]).toMatchObject({
      path: "/work/lib/core",
      label: "lib/core",
      branch: null,
      changedFiles: 0,
      hydrated: false,
    });
  });
});
