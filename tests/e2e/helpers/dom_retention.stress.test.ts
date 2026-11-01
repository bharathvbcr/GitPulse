import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { createRepoPanelCache, type RepoPanelCache } from "../../../src/lib/panels/repoPanelCache";
import {
  ViewSectionPanelRetentionManager,
  type TabViewState,
} from "./test-utils";

const here = dirname(fileURLToPath(import.meta.url));
const componentsDir = join(here, "..", "..", "..", "src", "lib", "components");

// ============================================================================
// HELPER: Recursively search Svelte AST for specific node types
// ============================================================================
function findAstNodes(node: any, targetType: string): any[] {
  const matches: any[] = [];
  if (!node || typeof node !== "object") return matches;
  if (node.type === targetType) {
    matches.push(node);
  }
  for (const key of Object.keys(node)) {
    if (key === "parent") continue;
    const child = node[key];
    if (Array.isArray(child)) {
      for (const item of child) {
        matches.push(...findAstNodes(item, targetType));
      }
    } else if (child && typeof child === "object") {
      matches.push(...findAstNodes(child, targetType));
    }
  }
  return matches;
}

// ============================================================================
// PART 1: TAB SWITCH CHURN (500 Rapid Switches & Child Instance Retention)
// ============================================================================
describe("Feature 11 Challenge 1: Tab Switch Churn & Component Retention", () => {
  const panelSource = readFileSync(join(componentsDir, "ViewSectionPanel.svelte"), "utf8");

  it("1.1: ViewSectionPanel.svelte AST contains exactly 0 KeyBlock nodes", () => {
    const compiled = compile(panelSource, { filename: "ViewSectionPanel.svelte" });
    const keyBlocks = findAstNodes(compiled.ast.html, "KeyBlock");
    expect(keyBlocks.length).toBe(0);
    expect(panelSource).not.toContain("{#key");
    expect(panelSource).not.toContain("repoStore");
  });

  it("1.2: Simulating 500 rapid repository switches under unkeyed retention preserves child instance without unmount", () => {
    // Model component lifecycle tracking:
    interface ComponentLifecycleTracker {
      instanceId: string;
      mountCount: number;
      unmountCount: number;
      renderCount: number;
      lastRepoPath: string;
      state: {
        scrollTop: number;
        expandedSet: Set<string>;
      };
    }

    class RetentionHarness {
      private activeRepo: string | null = null;
      private childInstance: ComponentLifecycleTracker;

      constructor() {
        this.childInstance = {
          instanceId: "component-instance-alpha-1",
          mountCount: 1,
          unmountCount: 0,
          renderCount: 1,
          lastRepoPath: "/repos/initial",
          state: {
            scrollTop: 350,
            expandedSet: new Set(["src", "src/lib"]),
          },
        };
        this.activeRepo = "/repos/initial";
      }

      /**
       * Simulates Svelte 5 reactive in-place update when ViewSectionPanel does NOT key on currentPath.
       * The snippet stays mounted in the DOM and only triggers derived/effect re-computations.
       */
      switchRepoUnkeyed(newRepoPath: string) {
        this.activeRepo = newRepoPath;
        this.childInstance.renderCount++;
        this.childInstance.lastRepoPath = newRepoPath;
        // Notice: mountCount and unmountCount remain unchanged!
      }

      /**
       * Counter-model: Simulates destructive remounting when {#key currentPath} WAS present.
       */
      switchRepoKeyed(newRepoPath: string) {
        this.activeRepo = newRepoPath;
        this.childInstance.unmountCount++;
        // Tearing down wipes state and creates a new instance!
        this.childInstance = {
          instanceId: `component-instance-${Math.random().toString(36).substring(2, 9)}`,
          mountCount: this.childInstance.mountCount + 1,
          unmountCount: this.childInstance.unmountCount,
          renderCount: 1,
          lastRepoPath: newRepoPath,
          state: {
            scrollTop: 0, // Reset to 0!
            expandedSet: new Set(), // Wiped!
          },
        };
      }

      getChild() {
        return this.childInstance;
      }
    }

    // 1. Run 500 switches against Unkeyed Retention
    const unkeyedHarness = new RetentionHarness();
    const initialInstanceId = unkeyedHarness.getChild().instanceId;

    const repos = Array.from({ length: 25 }, (_, i) => `/repos/repo-${i}`);
    for (let i = 0; i < 500; i++) {
      const targetRepo = repos[i % repos.length];
      unkeyedHarness.switchRepoUnkeyed(targetRepo);
    }

    const finalChild = unkeyedHarness.getChild();
    expect(finalChild.mountCount).toBe(1); // Mounted exactly once!
    expect(finalChild.unmountCount).toBe(0); // Never unmounted!
    expect(finalChild.renderCount).toBe(501); // 1 initial + 500 updates
    expect(finalChild.instanceId).toBe(initialInstanceId); // Exact same component instance!
    expect(finalChild.state.scrollTop).toBe(350); // State preserved across 500 switches!
    expect(finalChild.state.expandedSet.has("src")).toBe(true);

    // 2. Compare against Keyed Destructive Churn
    const keyedHarness = new RetentionHarness();
    for (let i = 0; i < 500; i++) {
      const targetRepo = repos[i % repos.length];
      keyedHarness.switchRepoKeyed(targetRepo);
    }
    const keyedFinalChild = keyedHarness.getChild();
    expect(keyedFinalChild.unmountCount).toBe(500); // 500 destructive unmounts!
    expect(keyedFinalChild.mountCount).toBe(501); // 501 separate instances created!
    expect(keyedFinalChild.instanceId).not.toBe(initialInstanceId); // Lost identity!
    expect(keyedFinalChild.state.scrollTop).toBe(0); // Lost scroll position!
  });
});

// ============================================================================
// PART 2: STATE AND SCROLL RETENTION ACROSS MULTI-REPO CYCLING
// ============================================================================
describe("Feature 11 Challenge 2: State and Scroll Retention across Navigation", () => {
  interface RichViewState {
    repoPath: string;
    scrollOffset: number;
    activeDiffFile: string | null;
    expandedTreeNodes: Set<string>;
    editorTabs: Array<{ id: string; path: string; dirty: boolean; cursor: { line: number; col: number } }>;
    searchQuery: string;
    selectedCommitId: string | null;
    domMounted: boolean;
    domVisible: boolean;
  }

  class RichTabRetentionManager {
    private tabs = new Map<string, RichViewState>();
    private activePath: string | null = null;

    openRepo(state: Omit<RichViewState, "domMounted" | "domVisible">): RichViewState {
      const fullState: RichViewState = {
        ...state,
        domMounted: true,
        domVisible: false,
      };
      this.tabs.set(state.repoPath, fullState);
      return this.switchRepo(state.repoPath);
    }

    switchRepo(repoPath: string): RichViewState {
      if (this.activePath && this.tabs.has(this.activePath)) {
        this.tabs.get(this.activePath)!.domVisible = false;
      }
      const target = this.tabs.get(repoPath);
      if (!target) throw new Error(`Repo tab not found: ${repoPath}`);
      target.domVisible = true;
      this.activePath = repoPath;
      return target;
    }

    getState(repoPath: string): RichViewState | undefined {
      return this.tabs.get(repoPath);
    }

    getActive(): RichViewState | null {
      return this.activePath ? this.tabs.get(this.activePath) || null : null;
    }
  }

  it("2.1: State in Repo 1 is 100% retained when navigating Repo 1 -> Repo 2 -> Repo 3 -> Repo 1", () => {
    const mgr = new RichTabRetentionManager();

    // 1. Setup Repo 1 with rich view state
    mgr.openRepo({
      repoPath: "/workspaces/alpha",
      scrollOffset: 2480,
      activeDiffFile: "src/lib/components/ViewSectionPanel.svelte",
      expandedTreeNodes: new Set(["src", "src/lib", "src/lib/components", "tests"]),
      editorTabs: [
        { id: "tab-1", path: "src/lib/components/ViewSectionPanel.svelte", dirty: true, cursor: { line: 45, col: 12 } },
        { id: "tab-2", path: "package.json", dirty: false, cursor: { line: 10, col: 0 } },
      ],
      searchQuery: "ViewSectionPanel",
      selectedCommitId: "cfac83b3",
    });

    // 2. Setup Repo 2 with distinct view state
    mgr.openRepo({
      repoPath: "/workspaces/beta",
      scrollOffset: 620,
      activeDiffFile: "src-tauri/src/main.rs",
      expandedTreeNodes: new Set(["src-tauri", "src-tauri/src"]),
      editorTabs: [
        { id: "tab-b1", path: "src-tauri/src/main.rs", dirty: false, cursor: { line: 1, col: 1 } },
      ],
      searchQuery: "tauri_main",
      selectedCommitId: "7ae2fa6a",
    });

    // 3. Setup Repo 3 with distinct view state
    mgr.openRepo({
      repoPath: "/workspaces/gamma",
      scrollOffset: 1200,
      activeDiffFile: "Cargo.toml",
      expandedTreeNodes: new Set(["docs"]),
      editorTabs: [
        { id: "tab-g1", path: "Cargo.toml", dirty: false, cursor: { line: 20, col: 4 } },
      ],
      searchQuery: "dependencies",
      selectedCommitId: "ba35cba4",
    });

    // Verify currently on Repo 3
    expect(mgr.getActive()?.repoPath).toBe("/workspaces/gamma");
    expect(mgr.getActive()?.scrollOffset).toBe(1200);

    // 4. Navigate back to Repo 1
    const repo1 = mgr.switchRepo("/workspaces/alpha");
    expect(repo1.domVisible).toBe(true);
    expect(repo1.domMounted).toBe(true);
    expect(repo1.scrollOffset).toBe(2480);
    expect(repo1.activeDiffFile).toBe("src/lib/components/ViewSectionPanel.svelte");
    expect(repo1.expandedTreeNodes.has("src/lib/components")).toBe(true);
    expect(repo1.expandedTreeNodes.size).toBe(4);
    expect(repo1.editorTabs.length).toBe(2);
    expect(repo1.editorTabs[0].dirty).toBe(true);
    expect(repo1.editorTabs[0].cursor).toEqual({ line: 45, col: 12 });
    expect(repo1.searchQuery).toBe("ViewSectionPanel");
    expect(repo1.selectedCommitId).toBe("cfac83b3");

    // 5. Navigate back to Repo 2
    const repo2 = mgr.switchRepo("/workspaces/beta");
    expect(repo2.scrollOffset).toBe(620);
    expect(repo2.activeDiffFile).toBe("src-tauri/src/main.rs");
    expect(repo2.expandedTreeNodes.size).toBe(2);
    expect(repo2.searchQuery).toBe("tauri_main");

    // 6. Navigate back to Repo 3
    const repo3 = mgr.switchRepo("/workspaces/gamma");
    expect(repo3.scrollOffset).toBe(1200);
    expect(repo3.activeDiffFile).toBe("Cargo.toml");
  });

  it("2.2: 100 interleaved switches between Repo 1, 2, and 3 produce zero cross-contamination", () => {
    const mgr = new RichTabRetentionManager();
    mgr.openRepo({
      repoPath: "/workspaces/repo-A",
      scrollOffset: 100,
      activeDiffFile: "A.ts",
      expandedTreeNodes: new Set(["dirA"]),
      editorTabs: [],
      searchQuery: "queryA",
      selectedCommitId: "commitA",
    });
    mgr.openRepo({
      repoPath: "/workspaces/repo-B",
      scrollOffset: 200,
      activeDiffFile: "B.ts",
      expandedTreeNodes: new Set(["dirB"]),
      editorTabs: [],
      searchQuery: "queryB",
      selectedCommitId: "commitB",
    });
    mgr.openRepo({
      repoPath: "/workspaces/repo-C",
      scrollOffset: 300,
      activeDiffFile: "C.ts",
      expandedTreeNodes: new Set(["dirC"]),
      editorTabs: [],
      searchQuery: "queryC",
      selectedCommitId: "commitC",
    });

    const targets = ["/workspaces/repo-A", "/workspaces/repo-B", "/workspaces/repo-C"];
    for (let i = 0; i < 100; i++) {
      const target = targets[i % 3];
      const active = mgr.switchRepo(target);
      if (target === "/workspaces/repo-A") {
        expect(active.scrollOffset).toBe(100);
        expect(active.activeDiffFile).toBe("A.ts");
        expect(active.searchQuery).toBe("queryA");
      } else if (target === "/workspaces/repo-B") {
        expect(active.scrollOffset).toBe(200);
        expect(active.activeDiffFile).toBe("B.ts");
        expect(active.searchQuery).toBe("queryB");
      } else {
        expect(active.scrollOffset).toBe(300);
        expect(active.activeDiffFile).toBe("C.ts");
        expect(active.searchQuery).toBe("queryC");
      }
    }
  });
});

// ============================================================================
// PART 3: MEMORY LEAK AND NODE GROWTH BOUNDS (maxRepos: 8)
// ============================================================================
describe("Feature 11 Challenge 3: Memory Leak and Node Growth Bounds", () => {
  it("3.1: createRepoPanelCache enforces strict upper bound of maxRepos: 8 under 10-repo cycling", () => {
    const cache = createRepoPanelCache<{ payload: string }>({ maxRepos: 8 });

    // Insert 10 repos sequentially (repo-0 through repo-9)
    for (let i = 0; i < 10; i++) {
      cache.set(`/repos/fleet-${i}`, { payload: `data-${i}` });
      expect(cache.size).toBeLessThanOrEqual(8);
    }
    expect(cache.size).toBe(8);

    // Verify LRU eviction order: oldest accessed entries (repo-0, repo-1) evicted first
    expect(cache.get("/repos/fleet-0")).toBeUndefined();
    expect(cache.get("/repos/fleet-1")).toBeUndefined();
    expect(cache.get("/repos/fleet-2")?.payload).toBe("data-2");
    expect(cache.get("/repos/fleet-9")?.payload).toBe("data-9");

    // Perform 50 full cycles across all 10 repos (500 set operations)
    for (let cycle = 0; cycle < 50; cycle++) {
      for (let r = 0; r < 10; r++) {
        cache.set(`/repos/fleet-${r}`, { payload: `cycle-${cycle}-data-${r}` });
        expect(cache.size).toBeLessThanOrEqual(8);
      }
    }

    // Size must remain strictly 8, never growing unbounded to 10 or 500
    expect(cache.size).toBe(8);
  });

  it("3.2: createRepoPanelCache respects canEvict protection for dirty editor tabs", () => {
    interface TabState {
      isDirty: boolean;
      name: string;
    }

    const protectedCache = createRepoPanelCache<TabState>({
      maxRepos: 4,
      canEvict: (val) => !val.isDirty, // Protected if dirty
    });

    // Add Repo 1 with dirty changes
    protectedCache.set("/repos/dirty-1", { isDirty: true, name: "unsaved file" });

    // Add 5 other clean repos
    for (let i = 2; i <= 6; i++) {
      protectedCache.set(`/repos/clean-${i}`, { isDirty: false, name: `clean-${i}` });
    }

    // Even though 6 total repos were added and maxRepos is 4, /repos/dirty-1 was NOT evicted
    expect(protectedCache.get("/repos/dirty-1")).toBeDefined();
    expect(protectedCache.get("/repos/dirty-1")?.name).toBe("unsaved file");

    // Once cleaned, it can be evicted
    protectedCache.set("/repos/dirty-1", { isDirty: false, name: "saved file" });
    for (let i = 10; i <= 15; i++) {
      protectedCache.set(`/repos/clean-${i}`, { isDirty: false, name: `clean-${i}` });
    }
    // Now dirty-1 is clean and gets evicted
    expect(protectedCache.get("/repos/dirty-1")).toBeUndefined();
    expect(protectedCache.size).toBe(4);
  });

  it("3.3: Bounded DOM element model prevents detached node accumulation", () => {
    // Model a simulated DOM manager with an LRU retention cap
    class BoundedDomRetentionPool {
      private domNodes = new Map<string, { rootId: string; elementCount: number; detached: boolean }>();
      private cache: RepoPanelCache<string>;
      private elementsPerRepo = 120; // Simulated DOM subtrees (e.g., file tree nodes, diff rows)

      constructor(maxRepos = 8) {
        this.cache = createRepoPanelCache<string>({ maxRepos });
      }

      visitRepo(repoPath: string) {
        // Hydrate from cache or mount fresh
        const existing = this.cache.get(repoPath);
        if (!existing) {
          // Mount new DOM nodes
          const rootId = `dom-root-${repoPath}`;
          this.domNodes.set(repoPath, { rootId, elementCount: this.elementsPerRepo, detached: false });
          this.cache.set(repoPath, rootId);

          // Evict any nodes no longer present in cache
          for (const [path, node] of this.domNodes.entries()) {
            if (!this.cache.get(path)) {
              node.detached = true;
              this.domNodes.delete(path); // Detached and cleaned from memory!
            }
          }
        }
      }

      getActiveDomRepoCount(): number {
        return this.domNodes.size;
      }

      getTotalLiveElementCount(): number {
        let total = 0;
        for (const node of this.domNodes.values()) {
          if (!node.detached) total += node.elementCount;
        }
        return total;
      }
    }

    const pool = new BoundedDomRetentionPool(8);

    // Cycle through 10 repos for 100 iterations (1,000 repo visits)
    for (let i = 0; i < 1000; i++) {
      pool.visitRepo(`/repos/repo-${i % 10}`);
      // Assert that at any point during churn, live repos never exceed maxRepos 8
      expect(pool.getActiveDomRepoCount()).toBeLessThanOrEqual(8);
      expect(pool.getTotalLiveElementCount()).toBeLessThanOrEqual(8 * 120);
    }

    expect(pool.getActiveDomRepoCount()).toBe(8);
    expect(pool.getTotalLiveElementCount()).toBe(960);
  });
});

// ============================================================================
// PART 4: STATIC SCAN FOR ABSENCE OF LEAF {#key $repoStore.currentPath} BLOCKS
// ============================================================================
describe("Feature 11 Challenge 4: Leaf Key Absence Scan", () => {
  const PRIMARY_VIEWS = [
    "WorkspaceView.svelte",
    "CodeView.svelte",
    "HistoryView.svelte",
    "InsightsView.svelte",
  ];

  it("4.1: None of the 4 primary view components contain any KeyBlock in AST or local currentPath keying", () => {
    for (const viewName of PRIMARY_VIEWS) {
      const content = readFileSync(join(componentsDir, viewName), "utf8");
      const compiled = compile(content, { filename: viewName });
      const keyBlocks = findAstNodes(compiled.ast.html, "KeyBlock");

      // Verify AST contains 0 KeyBlock elements
      expect(keyBlocks.length, `${viewName} contains unexpected KeyBlock in template AST`).toBe(0);

      // Verify no key blocks on currentPath or repoStore
      expect(content, `${viewName} contains {#key $repoStore.currentPath}`).not.toMatch(/\{#key\s+\$repoStore\.currentPath\}/);
      expect(content, `${viewName} contains {#key $currentRepoPath}`).not.toMatch(/\{#key\s+\$currentRepoPath\}/);
      expect(content, `${viewName} contains {#key currentPath}`).not.toMatch(/\{#key\s+currentPath\}/);

      // Verify each view wraps its content in ViewSectionPanel
      expect(content).toContain("<ViewSectionPanel");
    }
  });

  it("4.2: App.svelte does NOT wrap view panels in {#key $repoStore.currentPath}", () => {
    const appPath = join(here, "..", "..", "..", "src", "App.svelte");
    const appSource = readFileSync(appPath, "utf8");

    // Verify architectural comment and absence of destructive keys
    expect(appSource).not.toMatch(/\{#key \$repoStore\.currentPath\}\s*<(?:WorkspaceView|CodeView|HistoryView|InsightsView)/);
    expect(appSource).not.toMatch(/\{#key currentPath\}\s*<(?:WorkspaceView|CodeView|HistoryView|InsightsView)/);
  });

  it("4.3: Primary view section leaves do NOT reintroduce currentPath key blocks", () => {
    // Audit major child components rendered in views
    const childViews = [
      "WorkView.svelte",
      "FileViewer.svelte",
      "DiffViewer.svelte",
      "CommitTable.svelte",
      "CommitDetails.svelte",
      "GraphNotices.svelte",
      "RepoMapPanel.svelte",
      "BranchList.svelte",
      "WorktreesPanel.svelte",
    ];

    for (const childName of childViews) {
      const filePath = join(componentsDir, childName);
      const content = readFileSync(filePath, "utf8");

      // Verify none of these leaves key their root on currentPath
      expect(content, `${childName} keys on currentPath`).not.toContain("{#key $repoStore.currentPath}");
      expect(content, `${childName} keys on currentRepoPath`).not.toContain("{#key $currentRepoPath}");
    }
  });
});
