import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * Source text in a Git client has to be copyable.
 *
 * Both readers set `select-none` on their root so the toolbar, file rail and
 * gutters cannot be dragged into a selection. `user-select` inherits, so that
 * one class also made every line of every diff unselectable — in the pane
 * people copy from most. The gutters already carried their own `select-none`,
 * which only means anything once the text between them can be selected.
 *
 * These assertions are the shape of the fix, not its wording: the content
 * spans opt in, the gutters stay opted out, and the stylesheet grants it.
 */
const components = new URL("./", import.meta.url);
const source = (path: string) => readFileSync(join(components.pathname, path), "utf8");
const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

describe("diff and blame source text is selectable", () => {
  it("grants selection to the marked class in the stylesheet", () => {
    expect(css).toMatch(/\.gp-diff-text\s*\{[^}]*user-select:\s*text/);
  });

  it("marks every diff content span, unified and split", () => {
    const text = source("DiffViewer.svelte");
    // add / del / context in unified, plus left and right in split.
    const marked = text.match(/class="gp-diff-text min-w-0/g) ?? [];
    expect(marked).toHaveLength(5);
  });

  it("marks the blame source column", () => {
    expect(source("BlameViewer.svelte")).toContain('class="gp-diff-text px-3 whitespace-pre');
  });

  it("keeps the gutters unselectable so a drag copies code, not line numbers", () => {
    const text = source("DiffViewer.svelte");
    expect(text).toContain('text-textMuted/50 text-[10px] select-none shrink-0');
    // The +/- markers are diff notation and must stay out of a copied range.
    expect(text).toContain('dark:text-emerald-400 select-none font-bold shrink-0');
    expect(text).toContain('dark:text-rose-400 select-none font-bold shrink-0');
  });
});

describe("the two pointer gestures on a diff row stay separate", () => {
  it("lets a drag that starts on the code select text instead of staging lines", () => {
    // Line-range selection calls preventDefault, which suppresses native text
    // selection. Bailing out first when the gesture starts on the code is what
    // keeps both gestures usable on the same row.
    const text = source("DiffViewer.svelte");
    const handler = text.slice(
      text.indexOf("function onLinePointerDown"),
      text.indexOf("function onLinePointerEnter"),
    );
    const guard = handler.indexOf('closest?.(".gp-diff-text")');
    const prevent = handler.indexOf("event.preventDefault()");
    expect(guard).toBeGreaterThan(-1);
    expect(guard).toBeLessThan(prevent);
  });
});

describe("an explicit line selection can be copied", () => {
  it("offers a copy action beside the staging actions", () => {
    const text = source("DiffViewer.svelte");
    expect(text).toContain("onclick={copySelectedLines}");
  });

  it("strips the diff markers so the snippet pastes as code", () => {
    const text = source("DiffViewer.svelte");
    const fn = text.slice(
      text.indexOf("async function copySelectedLines"),
      text.indexOf("async function stageSelected"),
    );
    expect(fn).toContain("line.content.slice(1)");
  });
});
