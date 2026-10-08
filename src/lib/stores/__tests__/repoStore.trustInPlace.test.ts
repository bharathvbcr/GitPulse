/**
 * Two owners the task sheet leans on instead of re-deriving store rules:
 * trusting a waiting checkout without moving the reader (trustTab), and
 * knowing why an open would be refused before asking (openRefusal).
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";
import { createRepoStore, type InvokeFn } from "../repoStore";
import { memoryStorage, savePersistedWorkspace, WORKSPACE_VERSION, type StorageLike } from "../../repos/persist";
import { MAX_OPEN_TABS } from "../../repos/tabModel";
import { cancelPrompt, completePrompt, promptState } from "../modalStore";

function makeStore(trusted: Set<string>, storage: StorageLike = memoryStorage()) {
  const invoke: InvokeFn = async (cmd, args) => {
    const path = String(args?.repoPath ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        if (!trusted.has(path)) throw new Error(`REPOSITORY_TRUST_REQUIRED: ${path}`);
        return { path, name: path.split("/").pop(), is_bare: false, common_dir: `${path.replace(/\/\.claude\/worktrees\/.*$/, "")}/.git` } as never;
      case "cmd_repository_trust":
        return { path, scope: "none", worktrees: 1, identity: "id" } as never;
      case "cmd_grant_repository_trust":
        trusted.add(path);
        return undefined as never;
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
  return createRepoStore({
    invoke,
    storage,
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: { subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe, setSearch: () => {}, selectBranch: () => {}, clear: () => {} },
  });
}

afterEach(() => cancelPrompt());

describe("trustTab", () => {
  it("trusts and loads a waiting checkout without bringing it on screen", async () => {
    const trusted = new Set(["/code/app"]);
    const store = makeStore(trusted);
    await store.openRepo("/code/app");
    await store.openRepo("/code/app/.claude/worktrees/agent", { activate: false, deferTrust: true });
    const waiting = get(store).openTabs.find((tab) => tab.path.endsWith("agent"))!;
    expect(waiting.trustRequired).toBe(true);
    const active = get(store).activeTabId;

    const granting = store.trustTab(waiting.id);
    await vi.waitFor(() => expect(get(promptState)?.options.title).toBe("Trust this repository?"));
    completePrompt(true);
    expect(await granting).toBe(true);

    const after = get(store).openTabs.find((tab) => tab.id === waiting.id)!;
    expect(get(store).activeTabId).toBe(active);
    expect(after.trustRequired).toBe(false);
    // The grant carries the repository, so the worktree stacks with it.
    expect(after.family).toBeTruthy();
    expect(after.family).toBe(get(store).openTabs.find((tab) => tab.path === "/code/app")!.family);
  });

  it("does nothing for a tab that is not waiting, and keeps waiting when declined", async () => {
    const trusted = new Set(["/code/app"]);
    const store = makeStore(trusted);
    await store.openRepo("/code/app");
    expect(await store.trustTab(get(store).openTabs[0].id)).toBe(false);
    expect(get(promptState)).toBeNull();

    await store.openRepo("/code/other", { activate: false, deferTrust: true });
    const waiting = get(store).openTabs.find((tab) => tab.path === "/code/other")!;
    const declining = store.trustTab(waiting.id);
    await vi.waitFor(() => expect(get(promptState)).not.toBeNull());
    cancelPrompt();
    expect(await declining).toBe(false);
    expect(get(store).openTabs.find((tab) => tab.id === waiting.id)?.trustRequired).toBe(true);
  });
});

describe("openRefusal", () => {
  it("names the capacity refusal openRepo would give, and nothing for a checkout already open", async () => {
    const trusted = new Set<string>();
    for (let i = 0; i < MAX_OPEN_TABS; i += 1) trusted.add(`/code/r${i}`);
    // A full workspace arrives the way a real one does, by restore, which
    // reads only the repositories the watch pool holds.
    const storage = memoryStorage();
    savePersistedWorkspace(storage, {
      version: WORKSPACE_VERSION,
      tabs: Array.from({ length: MAX_OPEN_TABS }, (_, i) => ({
        path: `/code/r${i}`, pinned: false, viewTab: "work" as const, terminalOpen: false, searchQuery: "", selectedBranch: null,
      })),
      activePath: "/code/r0",
      recents: [],
      lastClosed: [],
    });
    const store = makeStore(trusted, storage);
    await store.restoreWorkspace();
    expect(get(store).openTabs).toHaveLength(MAX_OPEN_TABS);
    expect(store.openRefusal("/code/new")).toMatch(/Too many open repositories/);
    expect(store.openRefusal("/CODE/R3")).toBeNull();
    expect(store.openRefusal("")).toBe("Invalid repository path");
    trusted.add("/code/new");
    expect(await store.openRepo("/code/new")).toBe(false);
    expect(get(store).error).toBe(store.openRefusal("/code/new"));
  });
});
