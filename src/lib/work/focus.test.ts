import { describe, expect, it } from "vitest";
import type { BranchInfo, WorktreeInfo } from "../branches/types";
import type { PullRequestInfo } from "../github/types";
import type { FileStatus } from "../stores/repoStore";
import { emptyTally, type WorkRow } from "./projection";
import {
  filterWorkRows,
  hereSummary,
  rowInFacet,
  rowLastActivity,
  rowSearchText,
  WORK_FACETS,
} from "./focus";

function worktree(path: string, extra: Partial<WorktreeInfo> = {}): WorktreeInfo {
  return {
    path,
    name: path.split("/").pop() ?? path,
    head: "abc",
    branch: null,
    is_bare: false,
    is_detached: false,
    is_main: false,
    is_locked: false,
    is_prunable: false,
    dirty_files: 0,
    ...extra,
  };
}

function row(key: string, extra: Partial<WorkRow> = {}): WorkRow {
  return {
    key,
    kind: "worktree",
    taskId: "",
    title: key,
    status: "",
    lease: null,
    worktrees: [],
    pullRequests: [],
    runs: [],
    grants: [],
    verdicts: emptyTally(),
    operation: null,
    ...extra,
  };
}

function branch(name: string, extra: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name,
    is_current: false,
    is_remote: false,
    tip_commit_id: `tip-${name}`,
    ahead_count: 0,
    behind_count: 0,
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 0,
    last_author: "",
    last_summary: "",
    commits_ahead_of_base: 0,
    commits_behind_base: 0,
    additions: 0,
    deletions: 0,
    files_changed: 0,
    ...extra,
  };
}

function status(extra: Partial<FileStatus> = {}): FileStatus {
  return {
    path: "a.txt",
    status_code: "M",
    is_staged: false,
    is_conflicted: false,
    additions: 0,
    deletions: 0,
    ...extra,
  };
}

const pr = { number: 7, title: "add caching", head_ref: "feat-cache" } as PullRequestInfo;

const BLOCKED = row("blocked", {
  operation: {
    kind: "Rebase",
    current_step: 1,
    total_steps: 3,
    conflicted_total: 2,
  } as WorkRow["operation"] & object,
});
const AGENT = row("agent", {
  worktrees: [{ worktree: worktree("/repo/.claude/worktrees/x-1"), taskId: "", operation: null }],
});
const DIRTY = row("dirty", {
  worktrees: [{ worktree: worktree("/repo/wt", { dirty_files: 4, branch: "feat" }), taskId: "", operation: null }],
});
const WITH_PR = row("pr", { pullRequests: [pr] });
const ROWS = [BLOCKED, AGENT, DIRTY, WITH_PR];

describe("rowInFacet", () => {
  it("selects exactly the rows each tile counts", () => {
    expect(ROWS.filter((r) => rowInFacet(r, "all"))).toHaveLength(4);
    expect(ROWS.filter((r) => rowInFacet(r, "blocked"))).toEqual([BLOCKED]);
    expect(ROWS.filter((r) => rowInFacet(r, "agents"))).toEqual([AGENT]);
    expect(ROWS.filter((r) => rowInFacet(r, "dirty"))).toEqual([DIRTY]);
    expect(ROWS.filter((r) => rowInFacet(r, "pullRequests"))).toEqual([WITH_PR]);
  });

  it("never counts an unscanned worktree as dirty", () => {
    // dirty_files null means nobody looked. Folding that into "dirty" would
    // put a worktree nobody scanned into the list of ones known to be dirty.
    const unscanned = row("unscanned", {
      worktrees: [{ worktree: worktree("/repo/u", { dirty_files: null }), taskId: "", operation: null }],
    });
    expect(rowInFacet(unscanned, "dirty")).toBe(false);
  });

  it("covers every facet the strip can select", () => {
    for (const facet of WORK_FACETS) {
      expect(() => ROWS.filter((r) => rowInFacet(r, facet))).not.toThrow();
    }
  });
});

describe("rowSearchText", () => {
  it("matches on what the reader actually types: branch, path and pull request", () => {
    const text = rowSearchText(DIRTY);
    expect(text).toContain("feat");
    expect(text).toContain("/repo/wt");
    expect(rowSearchText(WITH_PR)).toContain("#7");
    expect(rowSearchText(WITH_PR)).toContain("add caching");
  });
});

describe("filterWorkRows", () => {
  it("applies facet and query together", () => {
    expect(filterWorkRows(ROWS, "all", "")).toHaveLength(4);
    expect(filterWorkRows(ROWS, "all", "caching")).toEqual([WITH_PR]);
    expect(filterWorkRows(ROWS, "blocked", "caching")).toEqual([]);
  });

  it("keeps the projection's order rather than re-sorting as the query changes", () => {
    const filtered = filterWorkRows(ROWS, "all", "");
    expect(filtered.map((r) => r.key)).toEqual(ROWS.map((r) => r.key));
  });

  it("treats a whitespace-only query as no query", () => {
    expect(filterWorkRows(ROWS, "all", "   ")).toHaveLength(4);
  });

  it("matches case-insensitively", () => {
    expect(filterWorkRows(ROWS, "all", "CACHING")).toEqual([WITH_PR]);
  });
});

describe("rowLastActivity", () => {
  const branches = [
    branch("feat", { last_commit_timestamp: 1000, last_author: "ada" }),
    branch("old", { last_commit_timestamp: 10 }),
  ];

  it("reports the most recent branch on the row", () => {
    const multi = row("m", {
      worktrees: [
        { worktree: worktree("/a", { branch: "old" }), taskId: "", operation: null },
        { worktree: worktree("/b", { branch: "feat" }), taskId: "", operation: null },
      ],
    });
    expect(rowLastActivity(multi, branches)).toEqual({
      branch: "feat",
      timestamp: 1000,
      author: "ada",
    });
  });

  it("says nothing rather than the epoch for a branch nobody measured", () => {
    const unknown = row("u", {
      worktrees: [{ worktree: worktree("/c", { branch: "ghost" }), taskId: "", operation: null }],
    });
    expect(rowLastActivity(unknown, branches)).toBeNull();
    expect(rowLastActivity(row("detached"), branches)).toBeNull();
  });

  it("never reads a remote branch of the same name", () => {
    const remoteOnly = [branch("feat", { is_remote: true, last_commit_timestamp: 999 })];
    const r = row("r", {
      worktrees: [{ worktree: worktree("/d", { branch: "feat" }), taskId: "", operation: null }],
    });
    expect(rowLastActivity(r, remoteOnly)).toBeNull();
  });
});

describe("hereSummary", () => {
  it("splits the working tree into staged, unstaged and conflicted", () => {
    const here = hereSummary(
      "feat",
      [branch("feat")],
      [
        status({ is_staged: true }),
        status({ path: "b.txt" }),
        status({ path: "c.txt", is_conflicted: true, is_staged: true }),
      ],
    );
    expect(here).toMatchObject({ staged: 1, unstaged: 1, conflicted: 1 });
  });

  it("carries the tracking state and the base comparison", () => {
    const here = hereSummary(
      "feat",
      [
        branch("feat", {
          upstream: "origin/feat",
          ahead_count: 3,
          behind_count: 1,
          commits_behind_base: 8,
          compared_to: "main",
        }),
      ],
      [],
    );
    expect(here).toMatchObject({
      upstream: { name: "origin/feat", ahead: 3, behind: 1, gone: false },
      behindBase: 8,
      comparedTo: "main",
      unmeasured: false,
    });
  });

  it("marks a branch the list has not carried yet as unmeasured, not as up to date", () => {
    // The branch list arrives progressively; rendering its pre-fetch zeroes
    // would tell the reader they are level with a remote nobody has asked
    // about.
    const here = hereSummary("feat", [], []);
    expect(here?.unmeasured).toBe(true);
    expect(here?.upstream).toBeNull();
  });

  it("returns nothing for a detached head rather than a branch named empty", () => {
    expect(hereSummary(null, [], [])).toBeNull();
  });
});
