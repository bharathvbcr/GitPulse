import { describe, expect, it } from "vitest";
import {
  computeTabLayout,
  normalizeGroupName,
  parentFolderName,
  type GroupHeaderItem,
  type TabItem,
} from "./tabGroups";
import type { OpenRepoTab } from "../stores/repoStore";

function makeTab(overrides: Partial<OpenRepoTab> & { id: string; path: string }): OpenRepoTab {
  return {
    id: overrides.id,
    path: overrides.path,
    name: overrides.name ?? "repo",
    label: overrides.label ?? "repo",
    pinned: overrides.pinned ?? false,
    group: overrides.group ?? null,
    isActive: overrides.isActive ?? false,
    isBare: overrides.isBare ?? false,
    isDirty: overrides.isDirty ?? false,
    isLoading: overrides.isLoading ?? false,
    error: overrides.error ?? null,
    currentBranch: overrides.currentBranch ?? "main",
    conflictedCount: overrides.conflictedCount ?? 0,
    changedCount: overrides.changedCount ?? 0,
  };
}

describe("normalizeGroupName", () => {
  it("normalizes and trims group names", () => {
    expect(normalizeGroupName("  devtools  ")).toBe("devtools");
    expect(normalizeGroupName("")).toBeNull();
    expect(normalizeGroupName("   ")).toBeNull();
    expect(normalizeGroupName(null)).toBeNull();
    expect(normalizeGroupName(undefined)).toBeNull();
    expect(normalizeGroupName(123)).toBeNull();
  });

  it("rejects strings with control characters", () => {
    expect(normalizeGroupName("devtools\x00group")).toBeNull();
    expect(normalizeGroupName("devtools\x1Fgroup")).toBeNull();
  });

  it("clamps long group names to 40 characters", () => {
    const longName = "a".repeat(60);
    const normalized = normalizeGroupName(longName);
    expect(normalized).toHaveLength(40);
    expect(normalized).toBe("a".repeat(40));
  });
});

describe("parentFolderName", () => {
  it("extracts parent folder from unix and windows paths", () => {
    expect(parentFolderName("/Users/dev/code/devtools/GitPulse")).toBe("devtools");
    expect(parentFolderName("/projects/web/frontend")).toBe("web");
    expect(parentFolderName("C:/Users/dev/repos/api")).toBe("repos");
  });

  it("returns null for top-level or root paths", () => {
    expect(parentFolderName("/repo")).toBeNull();
    expect(parentFolderName("/")).toBeNull();
    expect(parentFolderName("")).toBeNull();
  });
});

describe("computeTabLayout", () => {
  it("handles ungrouped tabs without adding group headers", () => {
    const tabs = [
      makeTab({ id: "t1", path: "/a/repo1", label: "repo1" }),
      makeTab({ id: "t2", path: "/b/repo2", label: "repo2" }),
    ];
    const layout = computeTabLayout(tabs, []);
    expect(layout.groups).toHaveLength(0);
    expect(layout.visibleItems).toHaveLength(2);
    expect(layout.visibleItems[0].kind).toBe("tab");
    expect(layout.visibleItems[1].kind).toBe("tab");
    expect(layout.visibleTabs).toHaveLength(2);
  });

  it("groups tabs under their group header", () => {
    const tabs = [
      makeTab({ id: "t1", path: "/code/devtools/repo1", group: "devtools" }),
      makeTab({ id: "t2", path: "/code/devtools/repo2", group: "devtools" }),
      makeTab({ id: "t3", path: "/code/other/repo3", group: null }),
    ];
    const layout = computeTabLayout(tabs, []);
    expect(layout.groups).toHaveLength(1);
    expect(layout.groups[0].group).toBe("devtools");
    expect(layout.groups[0].tabCount).toBe(2);
    expect(layout.groups[0].color).toBeNull();

    // visible items: 1 group header + 2 devtools tabs + 1 other tab
    expect(layout.visibleItems).toHaveLength(4);
    expect(layout.visibleItems[0].kind).toBe("group-header");
    expect((layout.visibleItems[0] as GroupHeaderItem).group).toBe("devtools");
    expect(layout.visibleItems[1].kind).toBe("tab");
    expect(layout.visibleItems[2].kind).toBe("tab");
    expect(layout.visibleItems[3].kind).toBe("tab");

    const colored = computeTabLayout(tabs, [], undefined, [{ group: "devtools", color: "teal" }]);
    expect(colored.groups[0].color).toBe("teal");
  });

  it("collapses tabs when their group is in collapsedGroups, keeping group head visible", () => {
    const tabs = [
      makeTab({ id: "t1", path: "/code/devtools/repo1", group: "devtools" }),
      makeTab({ id: "t2", path: "/code/devtools/repo2", group: "devtools" }),
      makeTab({ id: "t3", path: "/code/other/repo3", group: null }),
    ];
    const layout = computeTabLayout(tabs, ["devtools"]);
    expect(layout.groups[0].isCollapsed).toBe(true);

    // visible items: 1 collapsed group header + 1 ungrouped tab
    expect(layout.visibleItems).toHaveLength(2);
    expect(layout.visibleItems[0].kind).toBe("group-header");
    expect((layout.visibleItems[0] as GroupHeaderItem).isCollapsed).toBe(true);
    expect((layout.visibleItems[0] as GroupHeaderItem).tabCount).toBe(2);
    expect(layout.visibleItems[1].kind).toBe("tab");
    expect((layout.visibleItems[1] as TabItem).tab.id).toBe("t3");

    // visible tabs only contains repo3
    expect(layout.visibleTabs.map((t) => t.id)).toEqual(["t3"]);
  });

  it("handles 15 repos efficiently: 15 repos across 3 groups collapses to 3 group heads", () => {
    const tabs: OpenRepoTab[] = [];
    for (let i = 1; i <= 5; i++) {
      tabs.push(makeTab({ id: `dev-${i}`, path: `/code/devtools/d${i}`, group: "devtools" }));
    }
    for (let i = 1; i <= 5; i++) {
      tabs.push(makeTab({ id: `web-${i}`, path: `/code/web/w${i}`, group: "web" }));
    }
    for (let i = 1; i <= 5; i++) {
      tabs.push(makeTab({ id: `srv-${i}`, path: `/code/services/s${i}`, group: "services" }));
    }
    expect(tabs).toHaveLength(15);

    // When all 3 groups are collapsed:
    const layoutCollapsed = computeTabLayout(tabs, ["devtools", "web", "services"]);
    expect(layoutCollapsed.groups).toHaveLength(3);
    expect(layoutCollapsed.visibleItems).toHaveLength(3);
    expect(layoutCollapsed.visibleItems.every((it) => it.kind === "group-header")).toBe(true);
    expect(layoutCollapsed.visibleTabs).toHaveLength(0);

    // When only "web" is expanded:
    const layoutWebOpen = computeTabLayout(tabs, ["devtools", "services"]);
    expect(layoutWebOpen.visibleItems).toHaveLength(2 + 1 + 5); // 2 collapsed headers + 1 web header + 5 web tabs = 8 items
    expect(layoutWebOpen.visibleTabs).toHaveLength(5);
    expect(layoutWebOpen.visibleTabs.every((t) => t.group === "web")).toBe(true);
  });

  it("aggregates status flags onto group headers (active, dirty, conflicts, terminals)", () => {
    const terminalCounts = new Map<string, number>([
      ["/code/devtools/repo1", 2],
      ["/code/devtools/repo2", 1],
    ]);
    const tabs = [
      makeTab({ id: "t1", path: "/code/devtools/repo1", group: "devtools", isDirty: true, conflictedCount: 1 }),
      makeTab({ id: "t2", path: "/code/devtools/repo2", group: "devtools", isActive: true }),
    ];
    const layout = computeTabLayout(tabs, ["devtools"], terminalCounts);
    const header = layout.groups[0];
    expect(header.hasActiveTab).toBe(true);
    expect(header.isDirty).toBe(true);
    expect(header.conflictedCount).toBe(1);
    expect(header.terminalCount).toBe(3);
  });

  describe("collapse inactive tabs in groups", () => {
    it("collapses inactive tabs in a group while keeping the active tab visible", () => {
      const tabs = [
        makeTab({ id: "t1", path: "/code/devtools/repo1", group: "devtools", isActive: true }),
        makeTab({ id: "t2", path: "/code/devtools/repo2", group: "devtools" }),
        makeTab({ id: "t3", path: "/code/devtools/repo3", group: "devtools" }),
        makeTab({ id: "t4", path: "/code/other/repo4", group: null }),
      ];

      const layout = computeTabLayout(tabs, [], undefined, [], undefined, { collapseInactive: true });

      // Group header is not fully collapsed because it has an active tab
      expect(layout.groups[0].isCollapsed).toBe(false);
      expect(layout.groups[0].hasActiveTab).toBe(true);
      expect(layout.groups[0].tabCount).toBe(3);
      expect(layout.groups[0].collapsedInactiveCount).toBe(2);

      // Visible items: group-header + active tab t1 + ungrouped tab t4
      expect(layout.visibleItems).toHaveLength(3);
      expect(layout.visibleItems[0].kind).toBe("group-header");
      expect(layout.visibleItems[1].kind).toBe("tab");
      expect((layout.visibleItems[1] as TabItem).tab.id).toBe("t1");
      expect(layout.visibleItems[2].kind).toBe("tab");
      expect((layout.visibleItems[2] as TabItem).tab.id).toBe("t4");

      // Visible tabs only contains active tab t1 and ungrouped tab t4
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["t1", "t4"]);

      // Inactive tabs inside group are marked with isInsideCollapsedGroup
      const tabItems = layout.allItems.filter((it): it is TabItem => it.kind === "tab");
      expect(tabItems.find((it) => it.id === "t1")?.isInsideCollapsedGroup).toBe(false);
      expect(tabItems.find((it) => it.id === "t2")?.isInsideCollapsedGroup).toBe(true);
      expect(tabItems.find((it) => it.id === "t3")?.isInsideCollapsedGroup).toBe(true);
    });

    it("collapses all tabs into group header when a group has no active tab", () => {
      const tabs = [
        makeTab({ id: "dev1", path: "/code/dev/1", group: "devtools", isActive: true }),
        makeTab({ id: "dev2", path: "/code/dev/2", group: "devtools" }),
        makeTab({ id: "web1", path: "/code/web/1", group: "web" }),
        makeTab({ id: "web2", path: "/code/web/2", group: "web" }),
      ];

      const layout = computeTabLayout(tabs, [], undefined, [], undefined, { collapseInactive: true });

      const devHeader = layout.groups.find((g) => g.group === "devtools")!;
      const webHeader = layout.groups.find((g) => g.group === "web")!;

      expect(devHeader.isCollapsed).toBe(false);
      expect(devHeader.hasActiveTab).toBe(true);
      expect(devHeader.collapsedInactiveCount).toBe(1);

      expect(webHeader.isCollapsed).toBe(true);
      expect(webHeader.hasActiveTab).toBe(false);
      expect(webHeader.collapsedInactiveCount).toBe(2);

      // Visible items: devtools header + dev1 + web header
      expect(
        layout.visibleItems.map((it) =>
          it.kind === "group-header" ? it.group : it.kind === "tab" ? it.tab.id : it.id,
        ),
      ).toEqual([
        "devtools",
        "dev1",
        "web",
      ]);
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["dev1"]);
    });

    it("dynamically shifts visible tabs when active tab changes", () => {
      const makeTabsWithActive = (activeId: string) => [
        makeTab({ id: "dev1", path: "/code/dev/1", group: "devtools", isActive: activeId === "dev1" }),
        makeTab({ id: "dev2", path: "/code/dev/2", group: "devtools", isActive: activeId === "dev2" }),
        makeTab({ id: "web1", path: "/code/web/1", group: "web", isActive: activeId === "web1" }),
      ];

      // Switch to dev2
      const layoutDev2 = computeTabLayout(makeTabsWithActive("dev2"), [], undefined, [], undefined, { collapseInactive: true });
      expect(layoutDev2.visibleTabs.map((t) => t.id)).toEqual(["dev2"]);

      // Switch to web1
      const layoutWeb1 = computeTabLayout(makeTabsWithActive("web1"), [], undefined, [], undefined, { collapseInactive: true });
      expect(layoutWeb1.visibleTabs.map((t) => t.id)).toEqual(["web1"]);
      expect(layoutWeb1.groups.find((g) => g.group === "devtools")?.isCollapsed).toBe(true);
      expect(layoutWeb1.groups.find((g) => g.group === "web")?.isCollapsed).toBe(false);
    });

    it("supports per-group collapsedInactiveGroups when global option is false", () => {
      const tabs = [
        makeTab({ id: "dev1", path: "/code/dev/1", group: "devtools", isActive: true }),
        makeTab({ id: "dev2", path: "/code/dev/2", group: "devtools" }),
        makeTab({ id: "web1", path: "/code/web/1", group: "web" }),
        makeTab({ id: "web2", path: "/code/web/2", group: "web" }),
      ];

      const layout = computeTabLayout(tabs, [], undefined, [], undefined, {
        collapseInactive: false,
        collapsedInactiveGroups: ["devtools"],
      });

      // devtools has inactive collapsed (only dev1 visible)
      // web is fully expanded (web1 and web2 both visible)
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["dev1", "web1", "web2"]);
      expect(layout.groups.find((g) => g.group === "devtools")?.collapsedInactiveCount).toBe(1);
      expect(layout.groups.find((g) => g.group === "web")?.collapsedInactiveCount).toBe(0);
    });

    it("allows explicitly expanding a group even when global collapseInactive is true", () => {
      const tabs = [
        makeTab({ id: "dev1", path: "/code/dev/1", group: "devtools", isActive: true }),
        makeTab({ id: "dev2", path: "/code/dev/2", group: "devtools" }),
        makeTab({ id: "web1", path: "/code/web/1", group: "web" }),
        makeTab({ id: "web2", path: "/code/web/2", group: "web" }),
      ];

      const layout = computeTabLayout(tabs, [], undefined, [], undefined, {
        collapseInactive: true,
        expandedGroups: ["devtools"],
      });

      // devtools is explicitly expanded (dev1 and dev2 visible)
      // web has no active tab and global collapseInactive is on (web is collapsed)
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["dev1", "dev2"]);
      expect(layout.groups.find((g) => g.group === "devtools")?.collapsedInactiveCount).toBe(0);
      expect(layout.groups.find((g) => g.group === "web")?.isCollapsed).toBe(true);
    });

    it("collapses inactive tabs even when they have active shells (default behavior)", () => {
      const tabs = [
        makeTab({ id: "t1", path: "/code/devtools/repo1", group: "devtools", isActive: true }),
        makeTab({ id: "t2", path: "/code/devtools/repo2", group: "devtools" }), // has active shell
        makeTab({ id: "t3", path: "/code/devtools/repo3", group: "devtools" }),
      ];
      const terminalCounts = new Map([["/code/devtools/repo2", 2]]);

      // Default: keepActiveShells is false / undefined -> inactive tabs with shells are collapsed
      const layout = computeTabLayout(tabs, [], terminalCounts, [], undefined, {
        collapseInactive: true,
      });

      const header = layout.groups[0];
      expect(header.isCollapsed).toBe(false);
      expect(header.terminalCount).toBe(2);
      expect(header.collapsedInactiveCount).toBe(2);

      // Only the focused active tab t1 is visible
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["t1"]);
      expect(
        layout.visibleItems.map((it) => (it.kind === "group-header" ? it.group : it.id)),
      ).toEqual(["devtools", "t1"]);

      // Inactive tab with shells is marked as collapsed inside group
      const t2Item = layout.allItems.find((it): it is TabItem => it.kind === "tab" && it.id === "t2")!;
      expect(t2Item.isInsideCollapsedGroup).toBe(true);
    });

    it("collapses inactive groups containing active shells into group header with shell indicator", () => {
      const tabs = [
        makeTab({ id: "dev1", path: "/code/dev/1", group: "devtools", isActive: true }),
        makeTab({ id: "srv1", path: "/code/srv/1", group: "backend" }), // inactive with shell
        makeTab({ id: "srv2", path: "/code/srv/2", group: "backend" }),
      ];
      const terminalCounts = new Map([["/code/srv/1", 1]]);

      const layout = computeTabLayout(tabs, [], terminalCounts, [], undefined, {
        collapseInactive: true,
      });

      const backendHeader = layout.groups.find((g) => g.group === "backend")!;
      expect(backendHeader.isCollapsed).toBe(true);
      expect(backendHeader.terminalCount).toBe(1);
      expect(backendHeader.collapsedInactiveCount).toBe(2);

      // Only dev1 tab is visible; backend is collapsed into header showing terminalCount
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["dev1"]);
      expect(
        layout.visibleItems.map((it) => (it.kind === "group-header" ? it.group : it.id)),
      ).toEqual(["devtools", "dev1", "backend"]);
    });

    it("keeps tabs with active shells visible when keepActiveShells is true", () => {
      const tabs = [
        makeTab({ id: "t1", path: "/code/devtools/repo1", group: "devtools", isActive: true }),
        makeTab({ id: "t2", path: "/code/devtools/repo2", group: "devtools" }), // has active shell
        makeTab({ id: "t3", path: "/code/devtools/repo3", group: "devtools" }), // inactive no shell
      ];
      const terminalCounts = new Map([["/code/devtools/repo2", 1]]);

      const layout = computeTabLayout(tabs, [], terminalCounts, [], undefined, {
        collapseInactive: true,
        keepActiveShells: true,
      });

      const header = layout.groups[0];
      expect(header.isCollapsed).toBe(false);
      expect(header.terminalCount).toBe(1);
      expect(header.collapsedInactiveCount).toBe(1); // only t3 collapsed

      // Both t1 (active) and t2 (active shell) are visible
      expect(layout.visibleTabs.map((t) => t.id)).toEqual(["t1", "t2"]);
    });
  });
});
