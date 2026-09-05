import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { handleTablistKeydown, isTablistKey, panelId, tabId, tabProps } from "./tablist";

const read = (path: string) =>
  readFileSync(new URL(`../components/${path}`, import.meta.url), "utf8");

describe("tabProps", () => {
  it("keeps only the selected tab in the tab order", () => {
    // Every tab being tabbable is what makes a ten-tab strip cost ten presses
    // to step past; the arrow keys are how you move within a tablist.
    expect(tabProps("history", "graph", true).tabindex).toBe(0);
    expect(tabProps("history", "diff", false).tabindex).toBe(-1);
  });

  it("points each tab at the panel that exists for it", () => {
    const props = tabProps("history", "graph", true);
    expect(props["aria-controls"]).toBe(panelId("history", "graph"));
    expect(props.id).toBe(tabId("history", "graph"));
  });
});

describe("handleTablistKeydown", () => {
  it("moves with the horizontal arrows and wraps", () => {
    expect(handleTablistKeydown("ArrowRight", 0, 3)?.index).toBe(1);
    expect(handleTablistKeydown("ArrowRight", 2, 3)?.index).toBe(0);
    expect(handleTablistKeydown("ArrowLeft", 0, 3)?.index).toBe(2);
  });

  it("answers Home and End outright", () => {
    expect(handleTablistKeydown("Home", 2, 3)?.index).toBe(0);
    expect(handleTablistKeydown("End", 0, 3)?.index).toBe(2);
  });

  it("declines keys it does not own so the caller still sees them", () => {
    // A tablist that swallowed ArrowUp would break scrolling in the pane.
    for (const key of ["ArrowUp", "ArrowDown", "Enter", "a", "Tab"]) {
      expect(handleTablistKeydown(key, 0, 3), key).toBeNull();
      expect(isTablistKey(key)).toBe(false);
    }
  });

  it("declines when there are no tabs", () => {
    expect(handleTablistKeydown("ArrowRight", 0, 0)).toBeNull();
  });
});

describe("every tablist in the app implements the pattern it announces", () => {
  const TABLISTS: Array<[string, string]> = [
    ["ViewSectionBar.svelte", "section switcher"],
    ["ViewTabBar.svelte", "view switcher"],
    ["FileViewer.svelte", "open-file strip"],
  ];

  it.each(TABLISTS)("%s handles arrow keys", (file) => {
    const source = read(file);
    expect(source).toContain('role="tablist"');
    expect(source).toContain("handleTablistKeydown");
    expect(source).toContain("onkeydown={");
  });

  it.each(TABLISTS)("%s uses a roving tabindex", (file) => {
    const source = read(file);
    expect(source).toMatch(/tabindex=\{(props\.tabindex|isActive \? 0 : -1|active \? 0 : -1)\}/);
  });

  it.each(TABLISTS)("%s points its tabs at a panel", (file) => {
    expect(read(file)).toContain("aria-controls=");
  });

  it("has a real tabpanel for each of them", () => {
    // The defect: three tablists declared `role="tab"` and `aria-controls`
    // while no element in the app carried `role="tabpanel"` at all.
    expect(read("ViewSectionPanel.svelte")).toContain('role="tabpanel"');
    expect(read("FileViewer.svelte")).toContain('role="tabpanel"');
  });

  it("names the section tablist by its display label, not its raw id", () => {
    // The old accessible name announced "work sections".
    const source = read("ViewSectionBar.svelte");
    expect(source).toContain('aria-label="{groupLabel} sections"');
    expect(source).toContain("VIEW_REGISTRY[view].label");
  });
});
