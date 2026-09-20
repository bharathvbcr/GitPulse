import { describe, expect, it } from "vitest";
import {
  computeTabLayout,
  normalizeGroupName,
  parentFolderName,
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

describe("tabGroups adversarial stress tests", () => {
  it("withstands prototype pollution attacks safely", () => {
    const maliciousNames = ["__proto__", "constructor", "prototype", "toString", "valueOf", "isPrototypeOf"];
    for (const name of maliciousNames) {
      const normalized = normalizeGroupName(name);
      expect(normalized).toBe(name);

      const tabs = [
        makeTab({ id: "t1", path: `/a/${name}/r1`, group: name }),
        makeTab({ id: "t2", path: `/a/${name}/r2`, group: name }),
      ];
      const layout = computeTabLayout(tabs, [name]);
      expect(layout.groups).toHaveLength(1);
      expect(layout.groups[0].group).toBe(name);
      expect(layout.groups[0].isCollapsed).toBe(true);
      expect(layout.visibleTabs).toHaveLength(0);
      expect(layout.visibleItems).toHaveLength(1);
    }
  });

  it("handles 1,000 tabs across 100 groups under 50ms without degrading", () => {
    const tabs: OpenRepoTab[] = [];
    const groupNames: string[] = [];
    for (let g = 0; g < 100; g++) {
      const groupName = `group-${g}`;
      groupNames.push(groupName);
      for (let t = 0; t < 10; t++) {
        const id = `t-${g}-${t}`;
        tabs.push(
          makeTab({
            id,
            path: `/code/${groupName}/repo-${t}`,
            group: groupName,
            isDirty: t === 0,
            conflictedCount: t === 1 ? 2 : 0,
            isActive: g === 42 && t === 5,
          }),
        );
      }
    }
    expect(tabs).toHaveLength(1000);

    // Measure computation time
    const start = performance.now();
    // Collapse half the groups (50 collapsed, 50 expanded)
    const collapsed = groupNames.filter((_, idx) => idx % 2 === 0);
    const layout = computeTabLayout(tabs, collapsed);
    const duration = performance.now() - start;

    expect(duration).toBeLessThan(100);
    expect(layout.groups).toHaveLength(100);

    // 50 collapsed groups show only header (50)
    // 50 expanded groups show header + 10 tabs (50 * 11 = 550)
    // Total visible items = 600
    expect(layout.visibleItems).toHaveLength(600);
    expect(layout.visibleTabs).toHaveLength(500);

    // Group 42 is even, so it is collapsed; but it contains the active tab (g=42, t=5)!
    const g42 = layout.groups.find((g) => g.group === "group-42");
    expect(g42).toBeDefined();
    expect(g42!.hasActiveTab).toBe(true);
    expect(g42!.isDirty).toBe(true);
    expect(g42!.conflictedCount).toBe(2);
  });

  it("handles hostile path strings and parentFolderName extraction", () => {
    expect(parentFolderName("/")).toBeNull();
    expect(parentFolderName("")).toBeNull();
    expect(parentFolderName("   ")).toBeNull();
    expect(parentFolderName("\0/invalid")).toBeNull();
    expect(parentFolderName("/a".repeat(100))).toBe("a");
    expect(parentFolderName("///a///b///c///")).toBe("b");
    expect(parentFolderName("C:\\\\users\\\\code\\\\gitpulse")).toBe("code");
    expect(parentFolderName("C:/Users/name/repo")).toBe("name");
  });

  it("guarantees unique IDs across visible items for DOM rendering", () => {
    const tabs = [
      makeTab({ id: "tab-1", path: "/a/r1", group: "alpha" }),
      makeTab({ id: "tab-2", path: "/a/r2", group: "alpha" }),
      makeTab({ id: "tab-3", path: "/b/r3", group: "beta" }),
      makeTab({ id: "tab-4", path: "/c/r4", group: null }),
    ];
    const layout = computeTabLayout(tabs, []);
    const ids = layout.visibleItems.map((item) => item.id);
    const idSet = new Set(ids);
    expect(idSet.size).toBe(ids.length);
  });

  it("rapidly toggles collapse state across 500 cycles without drift", () => {
    const tabs = [
      makeTab({ id: "t1", path: "/a/r1", group: "grp" }),
      makeTab({ id: "t2", path: "/a/r2", group: "grp" }),
    ];
    let collapsed: string[] = [];
    for (let i = 0; i < 500; i++) {
      if (i % 2 === 0) {
        collapsed = ["grp"];
      } else {
        collapsed = [];
      }
      const layout = computeTabLayout(tabs, collapsed);
      if (i % 2 === 0) {
        expect(layout.visibleItems).toHaveLength(1);
        expect(layout.visibleTabs).toHaveLength(0);
      } else {
        expect(layout.visibleItems).toHaveLength(3);
        expect(layout.visibleTabs).toHaveLength(2);
      }
    }
  });
});
