import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "FileTreePanel.svelte"), "utf8");

describe("FileTreePanel", () => {
  it("lists repository files via cmd_list_repo_files with async guards", () => {
    expect(source).toContain('invoke<string[]>("cmd_list_repo_files"');
    expect(source).toContain("createAsyncGuard");
    expect(source).toContain("guard.isLive()");
  });

  it("provides status filter pills for all, modified, staged, untracked, conflicted", () => {
    expect(source).toContain("statusFilter");
    expect(source).toContain("'modified'");
    expect(source).toContain("'staged'");
    expect(source).toContain("'untracked'");
    expect(source).toContain("'conflicted'");
  });

  it("supports file operations (stage, unstage, discard, create file/folder)", () => {
    expect(source).toContain("stageFile");
    expect(source).toContain("unstageFile");
    expect(source).toContain("discardFile");
    expect(source).toContain("createNewFile");
    expect(source).toContain("createNewFolder");
  });

  it("supports external opening via Tauri plugin opener", () => {
    expect(source).toContain("openPath");
    expect(source).toContain("revealItemInDir");
  });

  it("uses VirtualList for tree rendering and supports keyboard navigation", () => {
    expect(source).toContain("VirtualList");
    expect(source).toContain("handleKeydown");
    expect(source).toContain('"ArrowDown"');
    expect(source).toContain('"ArrowUp"');
    expect(source).toContain('"Enter"');
  });

  it("filters with glob, regex, and fuzzy queries and reloads when status membership changes", () => {
    expect(source).toContain("parseFileQuery");
    expect(source).toContain("filterPathsByFileQuery");
    expect(source).toContain("statusPathKey");
    expect(source).toContain("joinWorktreePath");
    expect(source).toContain("onPinFile");
  });
});
