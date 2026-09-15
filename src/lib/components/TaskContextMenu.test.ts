import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskContextMenu.svelte", import.meta.url), "utf8");

describe("TaskContextMenu", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskContextMenu.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("portals a gp-menu, clamps to the viewport, and restores keyboard cycling", () => {
    expect(source).toContain("use:portal");
    expect(source).toContain("gp-menu");
    // Clamping and dismissal come from the shared popover owner; the phases
    // are held to account in popover.test.ts. The owner must be applied AFTER
    // the portal, or it measures the menu in the pane it is escaping.
    expect(source).toMatch(/use:portal=\{"body"\}\s*\n\s*use:popover=\{dismissal\}/);
    expect(source).toContain('kind: "point"');
    expect(source).toContain("inset: 8");
    expect(source).toContain("data-task-menu");
    expect(source).toContain('role="menu"');
    expect(source).toContain("ArrowDown");
    expect(source).toContain("cycleFocus");
    expect(source).toContain("menuPageItems");
    expect(source).toContain("Sparkles");
    expect(source).toContain("submenu");
  });

  it("re-measures when a submenu page changes the menu's height", () => {
    // Conditional items and submenu pages make any one clamp stale; the menu
    // would otherwise keep a position computed for a page it has left.
    expect(source).toContain("revision: `${items.length}:${page}`");
  });

  it("keeps Escape with the handler that owns the rest of the keyboard", () => {
    // Escape here backs out of a submenu before it closes anything, so the
    // owner must not also claim the key — two owners would fight over it.
    expect(source).toContain('escape: "none"');
    expect(source).toMatch(/if \(event\.key === "Escape"\)[\s\S]*?page = "root"/);
  });

  it("dismisses for every reason the anchor stops being where it was", () => {
    // A right-click elsewhere, an outside scroll, a resize. The scroll one is
    // judged by containment inside the owner, which is what keeps focusing a
    // row below the fold from dismissing the menu it is navigating.
    expect(source).toContain("contextmenu: true");
    expect(source).toContain("scroll: true");
    expect(source).toContain("resize: true");
    expect(source).toContain('inside: "[data-task-menu]"');
  });
});
