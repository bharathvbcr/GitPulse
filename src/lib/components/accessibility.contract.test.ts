import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const components = new URL("./", import.meta.url);
const source = (path: string) => readFileSync(join(components.pathname, path), "utf8");

describe("modal accessible-name contract", () => {
  it.each([
    ["CloneModal.svelte", "clone-modal-title"],
    ["RebaseModal.svelte", "rebase-modal-title"],
    ["CommandPalette.svelte", "command-palette-title"],
  ])("gives %s a title referenced by its dialog", (file, titleId) => {
    const text = source(file);
    expect(text).toContain(`aria-labelledby="${titleId}"`);
    expect(text).toContain(`id="${titleId}"`);
  });
});

describe("segmented-control semantics", () => {
  it("announces diff layout as a pressed-button group", () => {
    const text = source("DiffViewer.svelte");
    expect(text).toContain('class="gp-segmented" role="group" aria-label="Diff layout"');
    expect(text).toContain('aria-pressed={viewMode === "unified"}');
    expect(text).toContain('aria-pressed={viewMode === "split"}');
  });

  it("announces health filters as a pressed-button group", () => {
    const text = source("HealthPanel.svelte");
    expect(text).toContain('class="gp-segmented" role="group" aria-label="Vulnerability scope"');
    expect(text).toContain('aria-pressed={filter === "all"}');
    expect(text).toContain('aria-pressed={filter === "direct"}');
  });

  it("announces MANVI and Markdown modes as pressed-button groups", () => {
    const manvi = source("ManviOpsPanel.svelte");
    expect(manvi).toContain('class="gp-segmented" role="group" aria-label="MANVI view"');
    expect(manvi).toContain('aria-pressed={pane === "ops"}');
    expect(manvi).toContain('aria-pressed={pane === "harness"}');

    const markdev = source("files/MarkDevViewer.svelte");
    expect(markdev).toContain('class="gp-segmented" role="group" aria-label="Markdown view"');
    expect(markdev).toContain('aria-pressed={viewMode === "rendered"}');
    expect(markdev).toContain('aria-pressed={viewMode === "split"}');
    expect(markdev).toContain('aria-pressed={viewMode === "raw"}');
  });

  it("announces terminal modes as a pressed-button group", () => {
    const text = source("TerminalPanel.svelte");
    expect(text).toContain('class="gp-segmented" role="group" aria-label="Terminal mode"');
    expect(text).toContain('aria-pressed={mode === "shell"}');
    expect(text).toContain('aria-pressed={mode === "console"}');
    expect(text).not.toContain('role="tablist" aria-label="Terminal mode"');
  });

  it("announces image comparison modes as a pressed-button group", () => {
    const text = source("ImageDiffViewer.svelte");
    expect(text).toContain('class="gp-segmented" role="group" aria-label="Image comparison mode"');
    expect(text).toContain('aria-pressed={mode === "2up"}');
    expect(text).toContain('aria-pressed={mode === "swipe"}');
    expect(text).toContain('aria-pressed={mode === "onion"}');
    expect(text).toContain('aria-label="Swipe comparison divider"');
    expect(text).toContain('aria-label="After-image opacity"');
    expect(text).toContain('alt={`Before version of ${filePath}`}');
    expect(text).toContain('alt={`After version of ${filePath}`}');
  });
});
