import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * A page's full-width bar, toolbar, status strip, sub-header row or side rail
 * sits on the page's own ground: it paints no band tint. The tint read as a
 * darker separator across every page. Controls inside a bar keep their own
 * fills, and real cards, menus, modals and selection/hover states keep theirs,
 * so this pins the specific bars that lost a fill rather than banning
 * `bg-surface` across the app.
 */
const read = (relative: string) => readFileSync(new URL(relative, import.meta.url), "utf8");

/** The opening tag (or the `class:` directive's tag) that carries `anchor`. */
function tagWith(source: string, anchor: string): string {
  const at = source.indexOf(anchor);
  expect(at, `anchor not found: ${anchor}`).toBeGreaterThan(-1);
  return source.slice(source.lastIndexOf("<", at), source.indexOf(">", at) + 1);
}

const BARS: ReadonlyArray<readonly [file: string, anchors: readonly string[]]> = [
  ["../components/files/FileTreePanel.svelte", [
    "flex flex-col h-full font-sans text-xs min-h-0 overflow-hidden select-none",
    'shrink-0 h-9" data-tip-place',
    "overscroll-contain space-y-1.5",
  ]],
  ["../components/ContentSearch.svelte", ["data-content-search", "gp-section-edge flex flex-col gap-1.5 shrink-0"]],
  ["../components/FileViewer.svelte", ["data-file-path-strip"]],
  ["../components/files/CodeViewer.svelte", [
    "relative shrink-0 gp-section-edge select-none",
    "relative shrink-0 select-none",
    "gp-section-edge text-micro font-mono",
  ]],
  ["../components/files/MediaViewer.svelte", ['relative shrink-0 select-none" data-tip-place']],
  ["../components/files/MarkDevViewer.svelte", ['relative shrink-0 select-none" data-tip-place']],
  ["../components/RepoMapPanel.svelte", ["gap-x-3 gap-y-1 px-3 py-2 text-meta text-textMuted"]],
  ["../components/TerminalFilePane.svelte", [
    "px-2 h-8 select-none",
    "px-2.5 py-1 text-textMuted text-micro",
    "px-2.5 py-1 text-micro",
  ]],
  ["../components/TerminalPanel.svelte", [
    "px-3 py-1.5 flex flex-wrap gap-2 items-center shrink-0",
    'px-2 h-9 min-w-0"',
  ]],
  ["../components/TerminalSession.svelte", ["min-w-0 flex items-center gap-2 px-4 h-8"]],
  ["../components/TerminalDock.svelte", ["px-2.5 flex items-center gap-2 select-none"]],
  ["../components/CoverageViewer.svelte", ["data-coverage-family-strip", "px-3 py-1.5 font-sans flex flex-wrap"]],
  ["../components/WorkOverviewPane.svelte", ['data-testid="work-blocked-verdict"', 'data-testid="work-retained-drafts"']],
  ["../components/HealthPanel.svelte", ["px-4 py-1.5 flex flex-wrap items-center"]],
  ["../components/BlameViewer.svelte", [
    "px-4 py-2 flex items-center justify-between gap-3",
    "px-4 py-2 font-sans shrink-0 flex flex-col gap-1.5",
  ]],
  ["../components/DiffViewer.svelte", ["gap-y-1 px-3 py-1.5 font-sans", "gap-y-1 px-3 py-1 font-sans"]],
  ["../components/BlastRadiusPanel.svelte", ["gap-2 px-2.5 py-2"]],
  ["../components/GraphNotices.svelte", ['data-testid="graph-notices"']],
  ["../components/GraphClipNotice.svelte", ["shrink-0 px-2 py-1 text-micro"]],
  ["../components/FleetView.svelte", ["shrink-0 px-4 py-3 flex flex-col gap-3", "shrink-0 px-4 py-2 flex flex-wrap"]],
  ["../components/Sidebar.svelte", ["p-3 flex items-center justify-between gap-1"]],
  ["../components/BrowserSession.svelte", ['px-2 h-8" onsubmit']],
];

describe("page bars paint no band fill", () => {
  for (const [file, anchors] of BARS) {
    it(`${file.split("/").pop()}: bars and rails sit on the page's ground`, () => {
      const source = read(file);
      for (const anchor of anchors) {
        expect(tagWith(source, anchor), anchor).not.toMatch(/\bbg-surface(Hover)?(\/\d+)?\b/);
      }
    });
  }
});

/** Side columns that float as cards: a margin, a border and a radius from `gp-float-card`, stretched by their row. */
const CARDS: ReadonlyArray<readonly [file: string, anchor: string]> = [
  ["../components/FileViewer.svelte", '<FileTreeColumn class="gp-float-card"'],
  ["../components/FileViewer.svelte", "data-explorer-card"],
  ["../components/FileViewer.svelte", "data-live-pulse-card"],
  ["../components/CoverageViewer.svelte", "data-coverage-file-card"],
  ["../components/CoverageViewer.svelte", "data-coverage-analysis-card"],
  ["../components/RepoMapPanel.svelte", "data-repomap-subsystems-card"],
  ["../components/files/MarkDevViewer.svelte", "data-markdev-outline-card"],
  ["../components/TerminalFilePane.svelte", "data-terminal-file-explorer-card"],
  ["../components/AgentInbox.svelte", 'class="threads-pane gp-float-card"'],
  ["../components/ConflictEditor.svelte", 'class="file-rail gp-float-card"'],
];

describe("side columns are floating cards", () => {
  for (const [file, anchor] of CARDS) {
    it(`${file.split("/").pop()}: ${anchor}`, () => {
      const tag = tagWith(read(file), anchor);
      expect(tag).toContain("gp-float-card");
      // The card's margin plus `h-full` would overflow its row; it stretches.
      expect(tag).not.toMatch(/\bh-full\b/);
      expect(tag).not.toMatch(/\bbg-surface(\/\d+)?\b/);
    });
  }

  it("keeps the Coverage file list's widths", () => {
    expect(tagWith(read("../components/CoverageViewer.svelte"), "data-coverage-file-card")).toContain(
      "w-72 max-w-[40%] min-w-40 shrink-0",
    );
  });

  it("FileTreeColumn takes its float class from the caller and no longer forces h-full", () => {
    const column = read("../components/files/FileTreeColumn.svelte");
    const host = tagWith(column, "data-file-tree-column");
    expect(host).not.toMatch(/\bh-full\b/);
    expect(host).toContain("{className}");
  });

  it("the Tasks navigator floats unless it is the overlay, and the overlay keeps its own fill", () => {
    const board = read("../components/TaskBoard.svelte");
    expect(tagWith(board, 'id="task-scope-rail"')).toContain("class:gp-float-card={!overlay}");
    expect(board).toContain(".navigator{width:188px;flex-shrink:0;padding:10px 8px;overflow:auto}");
    expect(board).toMatch(/\.navigator\.is-overlay\{background:var\(--mac-fill-surface,rgb\(var\(--c-surface\)\)\);/);
  });

  it("the scoped rails keep no fill of their own", () => {
    const inbox = read("../components/AgentInbox.svelte");
    expect(inbox.slice(inbox.indexOf(".threads-pane {"), inbox.indexOf("}", inbox.indexOf(".threads-pane {")))).not.toContain("background");
    const conflict = read("../components/ConflictEditor.svelte");
    const rail = conflict.match(/\.file-rail \{[^}]*\}/)?.[0] ?? "";
    expect(rail).toContain("width: 210px");
    expect(rail).not.toContain("background");
  });
});

describe("the status bar sits on the window's ground", () => {
  const css = read("../../app.css");

  it("wears its own class, not the top rows' gp-repo-tabs", () => {
    const footer = tagWith(read("../components/StatusBar.svelte"), 'role="contentinfo"');
    expect(footer).toContain("gp-status-bar");
    expect(footer).toContain("bg-background");
    expect(footer).not.toContain("gp-repo-tabs");
    expect(footer).not.toMatch(/\bbg-surface(\/\d+)?\b/);
  });

  it("clears the macOS glass fill, restores the plate under reduced transparency and drops the sheen", () => {
    expect(css).toMatch(/html\.macos \.gp-glass\.gp-status-bar \{\s*background-color: transparent;\s*\}/);
    expect(css).toMatch(/html\.macos \.gp-glass\.gp-status-bar \{\s*background-color: rgb\(var\(--c-bg\)\);\s*\}/);
    expect(css).toMatch(/html\.macos\[data-transparency="reduced"\] \.gp-glass\.gp-status-bar \{\s*background-color: rgb\(var\(--c-bg\)\);\s*\}/);
    expect(css).toMatch(/html\.macos \.gp-glass\.gp-status-bar \{\s*background-image: none;\s*box-shadow: none;\s*\}/);
  });
});
