// The Diff view's two-commit comparison: what the store records and sends,
// and that every other selection clears it.
import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";
import type { FilterState } from "../filterStore";

function makeStore(onDiff: (cmd: string, args: Record<string, unknown>) => unknown) {
  const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo": return { path, name: "r", is_bare: false } as never;
      case "cmd_list_branches": return [] as never;
      case "cmd_get_status": return [] as never;
      case "cmd_list_tags": return { tags: [], truncated: false } as never;
      case "cmd_repo_operation": return null as never;
      case "cmd_stash_list": return { entries: [], truncated: false } as never;
      case "cmd_watch_repo": return path as never;
      case "cmd_unwatch_repo": case "cmd_note_tab_activated": case "cmd_set_recent_menu": return undefined as never;
      case "cmd_branch_stats":
        return { compared_to: "main", updates: [], computed: 0, cached: 0, capped: false, compute_failures: 0 } as never;
      default:
        calls.push({ cmd, args: args ?? {} });
        return onDiff(cmd, args ?? {}) as never;
    }
  };
  const filter = writable<FilterState>({ searchQuery: "", selectedBranch: null });
  const store = createRepoStore({
    invoke, storage: memoryStorage(), caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: filter.subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
  });
  return { store, calls };
}

const patch = (text: string) => ({ text, truncated: false });

describe("compare selection", () => {
  it("reads the direct change and records base and target", async () => {
    const { store, calls } = makeStore((cmd) => patch(`${cmd} diff`));
    await store.openRepo("/r/compare");
    await store.selectCompareDiff("base1", "target1");
    const state = get(store);
    expect(calls.at(-1)).toEqual({
      cmd: "cmd_get_compare_diff",
      args: { repoPath: "/r/compare", from: "base1", to: "target1", filePath: null },
    });
    expect(state.selectionKind).toBe("compare");
    expect(state.selectedCommitId).toBe("target1");
    expect(state.selectedCompareBase).toBe("base1");
    expect(state.selectedFilePath).toBeNull();
    expect(state.selectedDiff).toBe("cmd_get_compare_diff diff");
  });

  it("narrows to one file and keeps the comparison", async () => {
    const { store, calls } = makeStore(() => patch("one file"));
    await store.openRepo("/r/compare-file");
    await store.selectCompareFileDiff("base1", "target1", "src/a.ts");
    expect(calls.at(-1)?.args).toMatchObject({ from: "base1", to: "target1", filePath: "src/a.ts" });
    const state = get(store);
    expect(state.selectionKind).toBe("compare");
    expect(state.selectedCompareBase).toBe("base1");
    expect(state.selectedFilePath).toBe("src/a.ts");
  });

  it("is cleared by every other kind of selection", async () => {
    const { store } = makeStore(() => patch("x"));
    await store.openRepo("/r/compare-clear");
    for (const [label, next] of [
      ["commit", () => store.selectCommitDiff("c1")],
      ["commit file", () => store.selectCommitFileDiff("c1", "a.ts")],
      ["range", () => store.selectRangeDiff("main", "feat")],
      ["worktree file", () => store.selectFileDiff("a.ts")],
      ["inspect", async () => store.inspectCommitInHistory("c1")],
      ["file path", async () => store.selectFilePath("a.ts")],
    ] as const) {
      await store.selectCompareDiff("base1", "target1");
      expect(get(store).selectedCompareBase, label).toBe("base1");
      await next();
      expect(get(store).selectedCompareBase, label).toBeNull();
      expect(get(store).selectionKind, label).not.toBe("compare");
    }
  });

  it("drops a slower comparison's answer once a newer selection was made", async () => {
    let releaseFirst!: (value: unknown) => void;
    const { store } = makeStore((cmd, args) => {
      if (cmd === "cmd_get_compare_diff" && args.to === "slow") {
        return new Promise((resolve) => (releaseFirst = resolve));
      }
      return patch(`answer for ${String(args.to ?? args.commitId)}`);
    });
    await store.openRepo("/r/compare-race");
    const slow = store.selectCompareDiff("base1", "slow");
    await store.selectCommitDiff("fast");
    releaseFirst(patch("stale comparison"));
    await slow;
    const state = get(store);
    expect(state.selectedDiff).toBe("answer for fast");
    expect(state.selectedCompareBase).toBeNull();
    expect(state.selectionKind).toBe("commit");
  });

  it("reads a failed comparison as a failure, not as no difference", async () => {
    const { store } = makeStore(() => {
      throw new Error("bad object base1");
    });
    await store.openRepo("/r/compare-fail");
    await store.selectCompareDiff("base1", "target1");
    const state = get(store);
    expect(state.selectedDiffError).toContain("bad object");
    expect(state.selectedDiff).toBeNull();
    expect(state.selectionKind).toBe("compare");
    expect(state.selectedCompareBase).toBe("base1");
  });
});
