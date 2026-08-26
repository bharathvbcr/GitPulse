import { describe, expect, it } from "vitest";
import { VIEW_TABS } from "../repos/persist";
import {
  VIEW_NAV,
  flattenedViewNavTabs,
  formatViewTabLabel,
  isViewNavGroupActive,
  viewNavGroupFor,
  viewNavItemFor,
  viewNavPartitionsAllTabs,
  viewNavTriggerLabel,
} from "./viewNav";

describe("viewNav", () => {
  it("partitions every persisted view tab exactly once", () => {
    expect(viewNavPartitionsAllTabs()).toBe(true);
    expect([...flattenedViewNavTabs()].sort()).toEqual([...VIEW_TABS].sort());
  });

  it("keeps daily work as tabs and folds the rest into menus", () => {
    const work = VIEW_NAV.find((group) => group.id === "work");
    expect(work?.kind).toBe("tabs");
    expect(work?.items.map((item) => item.id)).toEqual(["history", "diff", "conflict"]);

    const menus = VIEW_NAV.filter((group) => group.kind === "menu");
    expect(menus.length).toBeGreaterThanOrEqual(1);
    const tabCount = VIEW_NAV.filter((group) => group.kind === "tabs").flatMap((group) => group.items)
      .length;
    expect(tabCount).toBeLessThan(VIEW_TABS.length);
    expect(tabCount).toBeLessThanOrEqual(3);
  });

  it("resolves group and item metadata for each tab", () => {
    expect(viewNavGroupFor("history")?.id).toBe("work");
    expect(viewNavGroupFor("coverage")?.id).toBe("inspect");
    expect(viewNavGroupFor("github")?.id).toBe("more");
    expect(viewNavGroupFor("terminal")?.id).toBe("more");
    expect(viewNavItemFor("history")?.label).toBe("Graph");
    expect(viewNavItemFor("conflict")?.label).toBe("Resolve");
    expect(viewNavItemFor("terminal")?.label).toBe("Terminal");
  });

  it("uses the active child as the menu trigger, otherwise the group label", () => {
    const inspect = VIEW_NAV.find((group) => group.id === "inspect")!;
    const more = VIEW_NAV.find((group) => group.id === "more")!;
    expect(viewNavTriggerLabel(inspect, "history")).toBe("Inspect");
    expect(viewNavTriggerLabel(inspect, "coverage")).toBe("Coverage");
    expect(viewNavTriggerLabel(more, "github")).toBe("GitHub");
    expect(isViewNavGroupActive(inspect, "blame")).toBe(true);
    expect(isViewNavGroupActive(inspect, "diff")).toBe(false);
  });

  it("only suffixes Resolve with the conflict count", () => {
    const resolve = viewNavItemFor("conflict")!;
    const graph = viewNavItemFor("history")!;
    expect(formatViewTabLabel(resolve, 0)).toBe("Resolve");
    expect(formatViewTabLabel(resolve, 3)).toBe("Resolve (3)");
    expect(formatViewTabLabel(graph, 3)).toBe("Graph");
  });
});
