import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

/**
 * What TaskBoard does is checked by rendering it: `harness/tasksChecks.js`
 * (run by `node scripts/browser-regressions.mjs --harness tasks`) drives the
 * real board in Chrome through drags, keys, menus, searches, archive, undo,
 * handoff and the editor sheet. This file keeps only what the source alone can
 * show, because a rendered page cannot see it:
 *
 * - it compiles without warnings;
 * - no rule paints an opaque fill over the shared material, and nothing here
 *   adds a backdrop filter per card;
 * - the board never wires an HTML5 drag, which is what Tauri's file-drop
 *   handler would intercept on the desktop shell (Chrome has no such handler);
 * - no native `window.confirm`, which blocks the webview.
 */
const source = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");

describe("TaskBoard source contracts", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskBoard.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("lets the shared material paint the board instead of covering it with opaque local fills", () => {
    for (const selector of ["workbench", "search", "column-title", "card", "ghost"]) {
      const rule = source.match(new RegExp(`\\.${selector}\\{([^}]*)\\}`))?.[1];
      expect(rule, `missing .${selector} rule`).toBeDefined();
      expect(rule, `.${selector} covers the shared material`).not.toMatch(/(?:^|;)background(?:-color)?:/);
    }
  });

  it("filters nothing per card", () => {
    expect(source).not.toContain("backdrop-filter:");
    expect(source).not.toMatch(/\bbackdrop-blur-/);
  });

  it("moves cards with pointer events, never an HTML5 drag that Tauri's file-drop would take over", () => {
    expect(source).not.toContain("ondragstart");
    expect(source).not.toContain("ondrop");
    expect(source).not.toContain("dataTransfer");
  });

  it("asks through the in-app dialog, never the native confirm", () => {
    expect(source).not.toContain("window.confirm");
  });
});
