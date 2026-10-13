// ft-038e6ba5: reads that fail must not look like empty or successful ones.
import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type BranchInfo, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";
import type { FilterState } from "../filterStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

function branch(name: string, tip: string, extras: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name, is_current: false, is_remote: false, remote_name: null, tip_commit_id: tip,
    ahead_count: 0, behind_count: 0, upstream: null, is_default: false, is_gone: false,
    last_commit_timestamp: 0, last_author: "ada", last_summary: "init",
    commits_ahead_of_base: 0, commits_behind_base: 0, additions: 0, deletions: 0, files_changed: 0,
    ...extras,
  };
}

function makeStore(extra: (cmd: string, args?: Record<string, unknown>) => unknown) {
  const state = { listed: [branch("main", "m1", { is_current: true, is_default: true }), branch("deleted-branch", "d1")] };
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo": return { path, name: "r", is_bare: false } as never;
      case "cmd_list_branches": return state.listed as never;
      case "cmd_get_status": return [] as never;
      case "cmd_list_tags": return { tags: [], truncated: false } as never;
      case "cmd_repo_operation": return null as never;
      case "cmd_stash_list": return { entries: [], truncated: false } as never;
      case "cmd_watch_repo": return path as never;
      case "cmd_unwatch_repo": case "cmd_note_tab_activated": case "cmd_set_recent_menu": return undefined as never;
      default: return extra(cmd, args) as never;
    }
  };
  const filter = writable<FilterState>({ searchQuery: "", selectedBranch: null });
  const store = createRepoStore({
    invoke, storage: memoryStorage(), caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: filter.subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
  });
  return { store, state };
}

const flush = async () => { await new Promise((r) => setTimeout(r, 0)); await new Promise((r) => setTimeout(r, 0)); };

describe("ft-038e6ba5 branch stats vs a newer branch list", () => {
  it("does not resurrect a deleted branch nor drop a new one when stats land after a refresh", async () => {
    const stats = deferred<unknown>();
    const { store, state } = makeStore((cmd) => {
      if (cmd === "cmd_branch_stats") return stats.promise;
      throw new Error(`unexpected ${cmd}`);
    });
    await store.openRepo("/r/stats-race");
    expect(get(store).branches.map((b) => b.name)).toEqual(["main", "deleted-branch"]);

    state.listed = [branch("main", "m1", { is_current: true, is_default: true }), branch("new-branch", "n1")];
    await store.refresh();
    expect(get(store).branches.map((b) => b.name)).toEqual(["main", "new-branch"]);

    stats.resolve({
      compared_to: "main",
      updates: [{ name: "main", tip_commit_id: "m1", is_remote: false, remote_name: null,
        additions: 5, deletions: 1, files_changed: 1, commits_ahead_of_base: 0, commits_behind_base: 0 }],
      computed: 1, cached: 0, capped: false,
    });
    await flush();
    const after = get(store).branches;
    expect(after.map((b) => b.name)).toEqual(["main", "new-branch"]);
    // The stats that did arrive still land on the branch they belong to.
    expect(after[0].additions).toBe(5);
  });
});

describe("ft-038e6ba5 a failed diff read", () => {
  const diff = (text: string) => ({ text, truncated: false, truncation_reason: null });

  it("lands on the selection, clears the previous diff, and leaves the status error alone", async () => {
    let fail = false;
    const { store } = makeStore((cmd, args) => {
      if (cmd === "cmd_branch_stats") return { compared_to: "main", updates: [], computed: 0, cached: 0, capped: false, compute_failures: 0 };
      if (cmd === "cmd_get_file_diff") {
        if (fail && args?.filePath === "b.txt") throw new Error("git diff failed: bad object");
        return diff(`diff for ${String(args?.filePath)}`);
      }
      throw new Error(`unexpected ${cmd}`);
    });
    await store.openRepo("/r/diff-fail");
    await store.selectFileDiff("a.txt");
    expect(get(store).selectedDiff).toBe("diff for a.txt");

    fail = true;
    await store.selectFileDiff("b.txt");
    const failed = get(store);
    expect(failed.error, "the repository status error is not the diff's").toBeNull();
    expect(failed.selectedDiffError).toContain("bad object");
    expect(failed.selectedDiff, "the previous file's diff must not stay under the new name").toBeNull();
    expect(failed.selectedFilePath).toBe("b.txt");
    expect(failed.selectedDiffPending).toBe(false);

    fail = false;
    await store.selectFileDiff("b.txt");
    const retried = get(store);
    expect(retried.selectedDiffError).toBeNull();
    expect(retried.selectedDiff).toBe("diff for b.txt");
  });

  it("every select*Diff reads failure the same way", async () => {
    const { store } = makeStore((cmd) => {
      if (cmd === "cmd_branch_stats") return { compared_to: "main", updates: [], computed: 0, cached: 0, capped: false, compute_failures: 0 };
      throw new Error("boom");
    });
    await store.openRepo("/r/diff-fail-all");
    for (const run of [
      () => store.selectCommitDiff("abc"),
      () => store.selectCommitFileDiff("abc", "f.txt"),
      () => store.selectRangeDiff("a", "b"),
    ]) {
      await run();
      const state = get(store);
      expect(state.error).toBeNull();
      expect(state.selectedDiffError).toContain("boom");
      expect(state.selectedDiff).toBeNull();
    }
  });
});
