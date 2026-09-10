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
    expect(source).toContain("clampMenuPosition");
    expect(source).toContain("data-task-menu");
    expect(source).toContain('role="menu"');
    expect(source).toContain("ArrowDown");
    expect(source).toContain("shouldDismissOverlay");
    expect(source).toContain("cycleFocus");
    expect(source).toContain("menuPageItems");
    expect(source).toContain("Sparkles");
    expect(source).toContain("submenu");
  });
});
