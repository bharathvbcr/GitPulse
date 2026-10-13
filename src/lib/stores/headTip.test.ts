import { writable } from "svelte/store";
import { describe, expect, it } from "vitest";
import { createHeadTipStore, headTip } from "./headTip";
import type { BranchInfo, RepoState } from "./repoStore";

function branch(name: string, tip: string, over: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name,
    is_current: false,
    is_remote: false,
    remote_name: null,
    tip_commit_id: tip,
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
    ...over,
  };
}

function repo(currentPath: string | null, branches: BranchInfo[]): Pick<RepoState, "currentPath" | "branches"> {
  return { currentPath, branches };
}

describe("headTip", () => {
  it("is the checked-out branch's tip on a branch, whatever the graph shows", () => {
    const state = repo("/r", [branch("main", "aaa", { is_current: true }), branch("topic", "bbb")]);
    expect(headTip(state, { visiblePath: "/r", headId: "zzz" })).toBe("aaa");
  });

  it("ignores a remote branch marked current", () => {
    const state = repo("/r", [branch("origin/main", "rrr", { is_current: true, is_remote: true })]);
    expect(headTip(state, { visiblePath: "/r", headId: "ddd" })).toBe("ddd");
  });

  it("falls back to the graph's HEAD when detached, only for the repository the graph shows", () => {
    // A detached HEAD has no current branch, so a checkout from one commit to
    // another would otherwise change nothing a panel keys on.
    const detached = repo("/r", [branch("main", "aaa")]);
    expect(headTip(detached, { visiblePath: "/r", headId: "ddd" })).toBe("ddd");
    expect(headTip(detached, { visiblePath: "/other", headId: "ddd" })).toBe("");
    expect(headTip(detached, { visiblePath: "/r", headId: null })).toBe("");
    expect(headTip(repo(null, []), { visiblePath: null, headId: "ddd" })).toBe("");
  });

  it("notifies only when HEAD moves, not on every publish of either store", () => {
    const repoState = writable(repo("/r", [branch("main", "aaa", { is_current: true })]));
    const graph = writable<{ visiblePath: string | null; headId: string | null }>({ visiblePath: "/r", headId: "aaa" });
    const seen: string[] = [];
    const stop = createHeadTipStore(repoState, graph).subscribe((tip) => seen.push(tip));

    // Fresh objects with the same HEAD: a status poll, a graph reload.
    repoState.set(repo("/r", [branch("main", "aaa", { is_current: true })]));
    graph.set({ visiblePath: "/r", headId: "aaa" });
    expect(seen).toEqual(["aaa"]);

    repoState.set(repo("/r", [branch("main", "bbb", { is_current: true })]));
    expect(seen).toEqual(["aaa", "bbb"]);
    stop();
  });
});
