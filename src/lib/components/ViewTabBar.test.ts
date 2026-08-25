import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ViewTabBar.svelte"),
  "utf8",
);

describe("ViewTabBar menu dismiss", () => {
  it("dismisses on capture-phase pointerdown outside the trigger and panel", () => {
    expect(source).toContain("shouldDismissOverlay");
    expect(source).toContain("[data-view-nav-menu], [data-view-nav-trigger]");
    expect(source).toContain('addEventListener("pointerdown", handlePointerDown, true)');
    expect(source).not.toContain('addEventListener("mousedown"');
  });

  it("does not treat the whole tablist as inside the menu", () => {
    expect(source).toContain("data-view-nav-trigger={group.id}");
    expect(source).toContain("data-view-nav-menu");
    expect(source).not.toMatch(/data-view-nav(?:\s|=|>)/);
  });

  it("does not close on nested scroll (header focus would dismiss on open)", () => {
    expect(source).not.toContain('addEventListener("scroll"');
    expect(source).toContain('addEventListener("resize", closeMenu)');
  });

  it("portals the panel to body so header overflow cannot clip or leak it", () => {
    expect(source).toContain('use:portal={"body"}');
  });
});
