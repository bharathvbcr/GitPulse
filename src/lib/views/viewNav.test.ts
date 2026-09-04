import { describe, expect, it } from "vitest";
import { VIEW_TABS } from "../repos/persist";
import {
  VIEW_NAV,
  formatViewTabLabel,
  viewNavCoversAllTabs,
  viewNavItemFor,
  viewNavTabs,
} from "./viewNav";

describe("viewNav", () => {
  it("lists every persisted view tab exactly once", () => {
    expect(viewNavCoversAllTabs()).toBe(true);
    expect([...viewNavTabs()].sort()).toEqual([...VIEW_TABS].sort());
  });

  it("puts every view in the header, in registry order", () => {
    // The old bound — "fewer tabs than views, so the title bar cannot grow a
    // button per panel" — was managing a symptom. Consolidation removed its
    // cause: a new panel is a section of the view that owns its subject, not
    // a fifth top-level entry. So every view is a tab, and the check that
    // matters is the ceiling on how many views there can be at all.
    expect(viewNavTabs()).toEqual(["work", "code", "history", "insights"]);
    expect(VIEW_NAV.length).toBe(VIEW_TABS.length);
    // Nine digit accelerators exist and the native menu asserts it too; a
    // header wider than that is the point at which grouping has to come back.
    expect(VIEW_NAV.length).toBeLessThanOrEqual(9);
  });

  it("resolves item metadata for each tab", () => {
    expect(viewNavItemFor("work")?.label).toBe("Work");
    expect(viewNavItemFor("code")?.label).toBe("Code");
    expect(viewNavItemFor("history")?.label).toBe("History");
    expect(viewNavItemFor("insights")?.label).toBe("Insights");
  });

  it("only suffixes Work — which owns Resolve now — with the conflict count", () => {
    const work = viewNavItemFor("work")!;
    const history = viewNavItemFor("history")!;
    expect(formatViewTabLabel(work, 0)).toBe("Work");
    expect(formatViewTabLabel(work, 3)).toBe("Work (3)");
    expect(formatViewTabLabel(history, 3)).toBe("History");
  });
});
