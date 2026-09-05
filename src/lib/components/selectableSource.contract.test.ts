import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
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
const here = dirname(fileURLToPath(import.meta.url));
const source = (path: string) => readFileSync(join(here, path), "utf8");
const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

describe("diff and blame source text is selectable", () => {
  it("grants selection to the marked class in the stylesheet", () => {
    expect(css).toMatch(/\.gp-diff-text\s*\{[^}]*user-select:\s*text/);
  });

  it("marks every diff content span, unified and split", () => {
    const text = source("DiffViewer.svelte");
    // This was five separate spans — add/del/context in unified plus left and
    // right in split — each needing the marker. Both layouts now render their
    // code through ONE `code` snippet, so the marker is asserted where it can
    // no longer be present on some rows and missing on others. Counting five
    // again would only be counting copies.
    const snippet = text.slice(text.indexOf("{#snippet code("));
    const span = snippet.slice(0, snippet.indexOf("{/snippet}"));
    expect(span).toContain('class="gp-diff-text min-w-0');
    // ...and both layouts must actually go through it.
    expect(text.match(/\{@render code\(/g) ?? []).toHaveLength(2);
  });

  it("marks the blame source column", () => {
    expect(source("BlameViewer.svelte")).toContain('class="gp-diff-text px-3 whitespace-pre');
  });

  it("keeps the gutters unselectable so a drag copies code, not line numbers", () => {
    const text = source("DiffViewer.svelte");
    // `select-none` sits on the gutter CLUSTER rather than on each of the line
    // number, stage box and +/- marker. user-select inherits, so opting the
    // container out covers a gutter element nobody has added yet — the three
    // per-element classes this replaces had to be remembered each time.
    const clusters = text.match(/class="flex[^"]*\bselect-none\b[^"]*self-stretch/g) ?? [];
    expect(clusters.length, "each layout's gutter cluster opts out").toBe(2);
    // The +/- markers are diff notation and must stay inside that cluster,
    // not out beside the code where a drag would collect them.
    for (const marker of text.match(/<span\s+class="w-2 shrink-0 text-center font-bold/g) ?? []) {
      expect(text.indexOf(marker)).toBeGreaterThan(-1);
    }
    expect(text.match(/w-2 shrink-0 text-center font-bold/g) ?? []).toHaveLength(2);
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
