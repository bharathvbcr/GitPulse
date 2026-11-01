import { describe, it, expect } from "vitest";
import path from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { get, writable } from "svelte/store";
import { createRepoPanelCache } from "../../../src/lib/panels/repoPanelCache";
import {
  createRepoStore,
  type BranchInfo,
  type FileStatus,
  type TagInfo,
  type StashEntry,
} from "../../../src/lib/stores/repoStore";
import { memoryStorage } from "../../../src/lib/repos/persist";
import {
  tabProps,
  handleTablistKeydown,
  tabId,
  panelId,
} from "../../../src/lib/dom/tablist";

const here = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(here, "../../..");

// Helper to construct a clean, isolated production repoStore backed by memoryStorage
function createTestRepoStore() {
  const invoke = async (cmd: string, args: Record<string, unknown> = {}) => {
    const repoPath = String(args?.repoPath ?? args?.path ?? "");
    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path: repoPath,
          name: repoPath.split("/").pop() || "repo",
          is_bare: false,
          common_dir: `${repoPath}/.git`,
        };
      case "cmd_get_status":
      case "cmd_list_branches":
        return [];
      case "cmd_list_tags":
        return { tags: [], truncated: false };
      case "cmd_stash_list":
        return { entries: [], truncated: false };
      case "cmd_branch_stats":
        return { compared_to: null, updates: [], computed: 0, cached: 0, capped: false };
      case "cmd_watch_repo":
        return repoPath;
      default:
        return undefined;
    }
  };

  return createRepoStore({
    invoke: invoke as any,
    storage: memoryStorage(),
    caseInsensitive: true,
    graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    filter: {
      subscribe: writable({ searchQuery: "", selectedBranch: null }).subscribe,
      setSearch: () => {},
      selectBranch: () => {},
      clear: () => {},
    },
    terminals: { countFor: () => 0, hostedFor: () => 0 },
  });
}

// ============================================================================
// PART 1: ADVERSARIAL STRESS-TESTING OF createRepoPanelCache
// ============================================================================
describe("Part 1: Adversarial Stress-Testing of createRepoPanelCache Memory Bounds & Eviction", () => {
  it("1.1: Strict memory capacity bound — 100 insertions into maxRepos=5 strictly never exceeds 5", () => {
    const cache = createRepoPanelCache<{ index: number }>({ maxRepos: 5 });

    for (let i = 0; i < 100; i++) {
      cache.set(`/repo/${i}`, { index: i });
      expect(cache.size).toBeLessThanOrEqual(5);
    }

    expect(cache.size).toBe(5);
    // Exactly the last 5 repos must be retained
    for (let i = 95; i < 100; i++) {
      expect(cache.get(`/repo/${i}`)).toEqual({ index: i });
    }
    // Previous repos must have been evicted
    for (let i = 0; i < 95; i++) {
      expect(cache.get(`/repo/${i}`)).toBeUndefined();
    }
  });

  it("1.2: LRU recency refresh on get() strictly prevents eviction of accessed victims", () => {
    const cache = createRepoPanelCache<string>({ maxRepos: 3 });

    cache.set("/repo/A", "stateA");
    cache.set("/repo/B", "stateB");
    cache.set("/repo/C", "stateC");

    // Reading /repo/A refreshes its recency, moving it from head (victim) to tail
    expect(cache.get("/repo/A")).toBe("stateA");

    // Adding /repo/D should evict /repo/B, NOT /repo/A
    cache.set("/repo/D", "stateD");

    expect(cache.size).toBe(3);
    expect(cache.get("/repo/A")).toBe("stateA"); // Retained!
    expect(cache.get("/repo/B")).toBeUndefined(); // Evicted!
    expect(cache.get("/repo/C")).toBe("stateC"); // Retained!
    expect(cache.get("/repo/D")).toBe("stateD"); // Retained!
  });

  it("1.3: canEvict protects pinned tabs from eviction and handles all-pinned bound gracefully", () => {
    interface PanelEntry {
      data: string;
      pinned: boolean;
    }

    const cache = createRepoPanelCache<PanelEntry>({
      maxRepos: 3,
      canEvict: (entry) => !entry.pinned,
    });

    // Insert 3 pinned entries
    cache.set("/repo/pin1", { data: "pin1", pinned: true });
    cache.set("/repo/pin2", { data: "pin2", pinned: true });
    cache.set("/repo/pin3", { data: "pin3", pinned: true });

    // Insert 2 unpinned entries
    cache.set("/repo/temp1", { data: "temp1", pinned: false });
    cache.set("/repo/temp2", { data: "temp2", pinned: false });

    // All 3 pinned entries must be preserved even though cache exceeds 3
    expect(cache.get("/repo/pin1")?.pinned).toBe(true);
    expect(cache.get("/repo/pin2")?.pinned).toBe(true);
    expect(cache.get("/repo/pin3")?.pinned).toBe(true);

    // Now insert another unpinned entry — should evict temp1
    cache.set("/repo/temp3", { data: "temp3", pinned: false });

    // Temp1 was evicted, but pinned entries remain safely intact
    expect(cache.get("/repo/temp1")).toBeUndefined();
    expect(cache.get("/repo/pin1")?.data).toBe("pin1");
  });

  it("1.4: Fuzzing: 5,000 rapid randomized get/set/clear operations preserve consistency", () => {
    const cache = createRepoPanelCache<{ count: number }>({ maxRepos: 6 });
    const repoPaths = Array.from({ length: 20 }, (_, i) => `/repo/fuzz/${i}`);
    const localMirror = new Map<string, number>();

    for (let step = 0; step < 5000; step++) {
      const op = Math.random();
      const pathIndex = Math.floor(Math.random() * repoPaths.length);
      const chosenPath = repoPaths[pathIndex];

      if (op < 0.6) {
        // Set
        const val = step;
        cache.set(chosenPath, { count: val });
        localMirror.set(chosenPath, val);
      } else if (op < 0.95) {
        // Get
        const res = cache.get(chosenPath);
        if (res !== undefined) {
          expect(res.count).toBe(localMirror.get(chosenPath));
        }
      } else {
        // Occasional clear
        cache.clear();
        localMirror.clear();
        expect(cache.size).toBe(0);
      }

      expect(cache.size).toBeLessThanOrEqual(6);
    }
  });

  it("1.5: Distinct cache instances remain completely isolated with zero state cross-contamination", () => {
    const diffCache = createRepoPanelCache<{ file: string }>({ maxRepos: 4 });
    const blameCache = createRepoPanelCache<{ line: number }>({ maxRepos: 4 });

    diffCache.set("/repo/shared", { file: "main.rs" });
    blameCache.set("/repo/shared", { line: 42 });

    expect(diffCache.get("/repo/shared")?.file).toBe("main.rs");
    expect(blameCache.get("/repo/shared")?.line).toBe(42);

    diffCache.clear();
    expect(diffCache.size).toBe(0);
    expect(blameCache.size).toBe(1);
    expect(blameCache.get("/repo/shared")?.line).toBe(42);
  });
});

// ============================================================================
// PART 2: ADVERSARIAL STRESS-TESTING OF createRepoStore TAB RETENTION
// ============================================================================
describe("Part 2: Adversarial Stress-Testing of createRepoStore Tab Retention & Accessibility", () => {
  it("2.1: Rapid switching: 200 consecutive tab switches maintain strict single-active invariant", async () => {
    const store = createTestRepoStore();
    const repoCount = 8;
    const paths = Array.from({ length: repoCount }, (_, i) => `/repo/rapid/${i}`);

    for (const p of paths) {
      await store.openRepo(p);
    }

    let state = get(store);
    expect(state.openTabs).toHaveLength(repoCount);
    const tabIds = state.openTabs.map((t) => t.id);

    // 200 rapid sequential and pseudo-random activations
    for (let i = 0; i < 200; i++) {
      const targetIndex = i % 2 === 0 ? (i / 2) % repoCount : Math.floor(Math.random() * repoCount);
      const targetId = tabIds[targetIndex];
      const targetPath = paths[targetIndex];

      await store.activateTab(targetId);

      state = get(store);
      expect(state.currentPath).toBe(targetPath);

      // Invariant: Exactly 1 tab is active, all others are inactive
      const activeTabs = state.openTabs.filter((t) => t.isActive);
      const inactiveTabs = state.openTabs.filter((t) => !t.isActive);

      expect(activeTabs).toHaveLength(1);
      expect(activeTabs[0].id).toBe(targetId);
      expect(activeTabs[0].path).toBe(targetPath);
      expect(inactiveTabs).toHaveLength(repoCount - 1);
      expect(inactiveTabs.every((t) => !t.isActive)).toBe(true);

      // Invariant: Retained sessions must stay positive
      expect(store.retainedSessionRecords()).toBeGreaterThan(0);
    }
  });

  it("2.2: Per-session state preservation: file selections and view lenses never leak across tabs", async () => {
    const store = createTestRepoStore();
    const repos = [
      { path: "/repo/mod-a", file: "src/a.ts", section: "code", subview: "blame" },
      { path: "/repo/mod-b", file: "src/b.ts", section: "code", subview: "explorer" },
      { path: "/repo/mod-c", file: "docs/c.md", section: "history", subview: "diff" },
      { path: "/repo/mod-d", file: "tests/d.rs", section: "insights", subview: "pulse" },
    ];

    for (const repo of repos) {
      await store.openRepo(repo.path);
      store.selectFilePath(repo.file);
      store.setViewSection(repo.section as any, repo.subview as any);
    }

    // Switch rapidly across tabs 40 times
    for (let round = 0; round < 40; round++) {
      const idx = round % repos.length;
      const expected = repos[idx];
      const tab = get(store).openTabs.find((t) => t.path === expected.path)!;
      await store.activateTab(tab.id);

      const state = get(store);
      expect(state.currentPath).toBe(expected.path);
      expect(state.selectedFilePath).toBe(expected.file);
      expect(state.viewSections[expected.section as keyof typeof state.viewSections]).toBe(expected.subview);
    }
  });

  it("2.3: Inactive tab sequence & ARIA accessibility invariants across 10 tabs", async () => {
    const store = createTestRepoStore();
    const count = 10;
    for (let i = 0; i < count; i++) {
      await store.openRepo(`/repo/a11y/${i}`);
    }

    const state = get(store);
    expect(state.openTabs).toHaveLength(count);

    // Verify ARIA tabProps for each tab in order
    let activeFound = 0;
    let inactiveFound = 0;

    for (const tab of state.openTabs) {
      const props = tabProps("repo", tab.id, tab.isActive);
      expect(props.role).toBe("tab");
      expect(props.id).toBe(tabId("repo", tab.id));
      expect(props["aria-controls"]).toBe(panelId("repo", tab.id));

      if (tab.isActive) {
        activeFound++;
        expect(props["aria-selected"]).toBe(true);
        expect(props.tabindex).toBe(0);
      } else {
        inactiveFound++;
        expect(props["aria-selected"]).toBe(false);
        expect(props.tabindex).toBe(-1);
      }
    }

    expect(activeFound).toBe(1);
    expect(inactiveFound).toBe(count - 1);

    // Keyboard roving focus stress test
    let currentIndex = 0;
    // Step forward 9 times with ArrowRight
    for (let step = 0; step < count - 1; step++) {
      const move = handleTablistKeydown("ArrowRight", currentIndex, count);
      expect(move).not.toBeNull();
      expect(move?.index).toBe(currentIndex + 1);
      currentIndex = move!.index;
    }
    expect(currentIndex).toBe(count - 1);

    // Jump to Home
    const homeMove = handleTablistKeydown("Home", currentIndex, count);
    expect(homeMove?.index).toBe(0);

    // Jump to End
    const endMove = handleTablistKeydown("End", 0, count);
    expect(endMove?.index).toBe(count - 1);
  });

  it("2.4: Tab teardown cascade: closing all tabs leaves zero lingering sessionBook memory records", async () => {
    const store = createTestRepoStore();
    const count = 6;
    for (let i = 0; i < count; i++) {
      await store.openRepo(`/repo/teardown/${i}`);
    }

    expect(get(store).openTabs).toHaveLength(count);
    const recordsInitial = store.retainedSessionRecords();
    expect(recordsInitial).toBeGreaterThan(0);

    // Close tabs one by one, alternating closing active and inactive
    while (get(store).openTabs.length > 0) {
      const state = get(store);
      const toClose = state.openTabs[0];
      const recordsBefore = store.retainedSessionRecords();

      await store.closeTab(toClose.id);

      const recordsAfter = store.retainedSessionRecords();
      expect(recordsAfter).toBeLessThanOrEqual(recordsBefore);
    }

    // All tabs closed: store must be clean and zero-leak
    const finalState = get(store);
    expect(finalState.openTabs).toHaveLength(0);
    expect(finalState.currentPath).toBeNull();
    expect(store.retainedSessionRecords()).toBe(0);
  });

  it("2.5: Idempotency & edge cases: reopening existing tab or activating nonexistent id does not corrupt state", async () => {
    const store = createTestRepoStore();
    await store.openRepo("/repo/alpha");
    await store.openRepo("/repo/beta");

    expect(get(store).openTabs).toHaveLength(2);

    // Reopen /repo/alpha: must activate existing tab without duplicate creation
    const reopened = await store.openRepo("/repo/alpha");
    expect(reopened).toBe(true);
    expect(get(store).openTabs).toHaveLength(2);
    expect(get(store).currentPath).toBe("/repo/alpha");

    // Activate nonexistent tab id: does not crash or corrupt active tab
    await store.activateTab("invalid-uuid-tab");
    expect(get(store).currentPath).toBe("/repo/alpha");
    expect(get(store).openTabs).toHaveLength(2);

    // Close nonexistent tab id: no-op
    await store.closeTab("invalid-uuid-tab");
    expect(get(store).openTabs).toHaveLength(2);
  });

  it("2.6: Production component architectural invariant: zero destructive #key blocks across view tree", () => {
    const filesToCheck = [
      "src/lib/components/ViewSectionPanel.svelte",
      "src/lib/components/WorkspaceView.svelte",
      "src/lib/components/CodeView.svelte",
      "src/lib/components/HistoryView.svelte",
      "src/lib/components/InsightsView.svelte",
      "src/App.svelte",
    ];

    for (const relPath of filesToCheck) {
      const absPath = path.join(projectRoot, relPath);
      expect(existsSync(absPath)).toBe(true);
      const content = readFileSync(absPath, "utf8").replace(/<!--[\s\S]*?-->/g, "");

      // Invariant: no destructive keying on currentPath
      expect(content).not.toContain("{#key $repoStore.currentPath}");
      expect(content).not.toContain("{#key currentPath}");
      if (relPath.includes("ViewSectionPanel")) {
        expect(content).not.toContain("{#key");
      }
    }
  });
});
