import { describe, expect, it } from "vitest";
import { VIEW_TABS, type ViewTab } from "../repos/persist";
import { REGISTERED_VIEWS, VIEW_REGISTRY, viewTabForMenuId } from "./viewRegistry";

describe("viewRegistry", () => {
  it("registers exactly the persisted view tabs, in both directions", () => {
    const registered = Object.keys(VIEW_REGISTRY);
    // Same members both ways; declaration order is a display concern and is
    // pinned separately below.
    expect([...registered].sort()).toEqual([...VIEW_TABS].sort());
    expect(new Set(registered)).toEqual(new Set(VIEW_TABS));
  });

  it("gives every view a label and a header group", () => {
    for (const view of REGISTERED_VIEWS) {
      expect(view.label.length).toBeGreaterThan(0);
      expect(view.menuGroup).toBeDefined();
      expect(view.id.length).toBeGreaterThan(0);
    }
  });

  it("includes manvi (regression: it once existed as a tab but was missing from consumers)", () => {
    const manvi = VIEW_REGISTRY.manvi;
    expect(manvi.label).toBe("MANVI");
    expect(manvi.menuGroup).toBe("more");
  });

  it("resolves every registered tab from its native menu id and nothing else", () => {
    for (const tab of VIEW_TABS) {
      expect(viewTabForMenuId(`tab-${tab}`)).toBe(tab);
    }
    expect(viewTabForMenuId("tab-nonexistent")).toBeUndefined();
    expect(viewTabForMenuId("tab-")).toBeUndefined();
    expect(viewTabForMenuId("open")).toBeUndefined();
    expect(viewTabForMenuId("")).toBeUndefined();
  });

  it("keeps declaration order stable so menus render predictably", () => {
    expect(REGISTERED_VIEWS.map((view) => view.id)).toEqual([
      "work",
      "files",
      "history",
      "diff",
      "conflict",
      "blame",
      "coverage",
      "health",
      "storage",
      "stack",
      "pulse",
      "terminal",
      "manvi",
      "github",
      "reflog",
    ] satisfies ViewTab[]);
  });
});
