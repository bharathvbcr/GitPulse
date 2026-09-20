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

    // visible items: 1 group header + 2 devtools tabs + 1 other tab
    expect(layout.visibleItems).toHaveLength(4);
    expect(layout.visibleItems[0].kind).toBe("group-header");
    expect((layout.visibleItems[0] as GroupHeaderItem).group).toBe("devtools");
    expect(layout.visibleItems[1].kind).toBe("tab");
    expect(layout.visibleItems[2].kind).toBe("tab");
    expect(layout.visibleItems[3].kind).toBe("tab");
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
});
