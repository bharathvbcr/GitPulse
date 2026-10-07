/**
 * The close guard counts a tab's live terminals under the store's own path
 * identity, not a second, platform-guessed one. A store told the volume is
 * case-insensitive must treat `/R/Alpha` and `/r/alpha` as one checkout here
 * exactly as it does for its tabs; it used to fall back to whatever the
 * runtime reported, so the guard and the strip could disagree about which
 * repository a shell was in.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage } from "../../repos/persist";
import { terminalSessions } from "../../terminal/sessionRegistry";
import { cancelPrompt, promptState } from "../modalStore";

const invoke: InvokeFn = async (cmd, args) => {
  const path = String(args?.repoPath ?? "");
  switch (cmd) {
    case "cmd_resolve_repo":
      return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path}/.git` } as never;
    case "cmd_list_branches":
    case "cmd_get_status":
      return [] as never;
    case "cmd_list_tags":
      return { tags: [], truncated: false } as never;
    case "cmd_repo_operation":
      return null as never;
    case "cmd_stash_list":
      return { entries: [], truncated: false } as never;
    case "cmd_branch_stats":
      return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false } as never;
    case "cmd_watch_repo":
      return path as never;
    default:
      return undefined as never;
  }
};

function makeStore(caseInsensitive: boolean) {
  return createRepoStore({
    invoke,
    storage: memoryStorage(),
    caseInsensitive,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: {
      subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe,
      setSearch: () => {},
      selectBranch: () => {},
      clear: () => {},
    },
  });
}

let release: (() => void) | null = null;
afterEach(() => {
  cancelPrompt();
  release?.();
  release = null;
});

describe("the close guard's terminal count", () => {
  it("counts a shell recorded under another spelling of the tab's checkout on a case-insensitive store", async () => {
    const slot = terminalSessions.reserve({ key: "identity-a", repoPath: "/R/Alpha", label: "Shell", status: "running", close: async () => {} });
    release = () => slot.release();
    const store = makeStore(true);
    await store.openRepo("/r/alpha");
    const closing = store.closeTab(get(store).activeTabId!);
    await vi.waitFor(() => expect(get(promptState)?.options.title).toBe("End the terminal session?"));
    cancelPrompt();
    await closing;
    expect(get(store).openTabs).toHaveLength(1);
  });

  it("does not count it on a case-sensitive store, where they are two checkouts", async () => {
    const slot = terminalSessions.reserve({ key: "identity-b", repoPath: "/R/Alpha", label: "Shell", status: "running", close: async () => {} });
    release = () => slot.release();
    const store = makeStore(false);
    await store.openRepo("/r/alpha");
    await store.closeTab(get(store).activeTabId!);
    expect(get(promptState)).toBeNull();
    expect(get(store).openTabs).toEqual([]);
  });
});
