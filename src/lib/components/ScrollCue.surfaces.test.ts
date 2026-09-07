import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Chrome whose overflow used to be silent (overlay scrollbars, hidden
 * header tracks, a graph gutter with only a fade) must go through ScrollCue
 * or VirtualList's scrollCue opt-in. A new strip that hides its overflow
 * without an arrow is a regression of the advertisement, not a styling
 * choice.
 */
const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "../..");

function read(rel: string): string {
  return readFileSync(join(root, rel), "utf8");
}

describe("overflow arrows on chrome scrollers", () => {
  it.each([
    ["lib/components/RepoTabBar.svelte", 'axis="x"'],
    ["lib/components/CommitTable.svelte", 'axis="x"'],
    ["lib/components/Sidebar.svelte", 'axis="y"'],
    ["lib/components/FileViewer.svelte", 'axis="x"'],
    ["App.svelte", 'axis="x"'],
    ["lib/components/BranchList.svelte", 'axis="x"'],
    ["lib/components/BranchList.svelte", 'axis="y"'],
    ["lib/components/TerminalPanel.svelte", 'axis="x"'],
    ["lib/components/DiffFileRail.svelte", 'axis="y"'],
  ] as const)("%s mounts ScrollCue on the %s axis", (file, axis) => {
    const source = read(file);
    expect(source).toContain("ScrollCue");
    expect(source).toContain(axis);
  });

  it("opts the file explorer and diff file list into VirtualList's cue", () => {
    expect(read("lib/components/files/FileTreePanel.svelte")).toContain("scrollCue");
    expect(read("lib/components/DiffFileRail.svelte")).toContain("scrollCue");
    expect(read("lib/components/VirtualList.svelte")).toContain("scrollCue = false");
  });

  it("does not put a cue on source panes that already have a real scrollbar", () => {
    expect(read("lib/components/DiffViewer.svelte")).not.toContain("scrollCue");
    expect(read("lib/components/BlameViewer.svelte")).not.toContain("scrollCue");
    expect(read("lib/components/files/CodeViewer.svelte")).not.toContain("scrollCue");
  });
});
