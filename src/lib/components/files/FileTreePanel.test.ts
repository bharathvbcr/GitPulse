import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

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

  /**
   * Rust flags a row whose numstat record it could not parse, meaning the
   * +/- counts understate the file. That field crossed IPC from the day it
   * was added and no TypeScript property received it, so a possibly-wrong
   * count rendered identically to a verified one.
   */
  it("marks churn counts the backend could not verify", () => {
    expect(source).toContain("status.warnings ?? []");
    // A distinct colour and a tilde, so the uncertainty survives without hover.
    expect(source).toContain("text-amber-400");
    expect(source).toContain('churnWarnings.length > 0 ? "~" : ""');
    // And the reasons themselves, not just the fact that there were reasons.
    expect(source).toContain('churnWarnings.join("; ")');
  });

  it("maps staged files to unstage and unstaged files to stage", () => {
    const menuStart = source.indexOf("{#if status.is_staged}");
    const menuEnd = source.indexOf(
      '<div class="my-1 border-t border-border/60"></div>',
      menuStart,
    );
    const statusMenu = source.slice(menuStart, menuEnd);
    const elseIndex = statusMenu.indexOf("{:else}");
    const stagedBranch = statusMenu.slice(0, elseIndex);
    const unstagedBranch = statusMenu.slice(elseIndex);

    expect(menuStart).toBeGreaterThan(-1);
    expect(menuEnd).toBeGreaterThan(menuStart);
    expect(elseIndex).toBeGreaterThan(-1);
    expect(stagedBranch).toContain("onclick={() => unstageFile(row.path)}");
    expect(stagedBranch).toContain("Unstage Changes");
    expect(stagedBranch).not.toContain("onclick={() => stageFile(row.path)}");
    expect(unstagedBranch).toContain("onclick={() => stageFile(row.path)}");
    expect(unstagedBranch).toContain("Stage File");
    expect(unstagedBranch).not.toContain("onclick={() => unstageFile(row.path)}");
  });

  it("supports discard and create file/folder operations", () => {
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

  it("implements the complete single-select tree navigation contract", () => {
    const handler = source.slice(
      source.indexOf("function handleKeydown"),
      source.indexOf("function ensureVisible"),
    );

    expect(handler).toContain('e.key === "Home"');
    expect(handler).toContain('e.key === "End"');
    expect(handler).toContain("moveTreeSelection(0)");
    expect(handler).toContain("moveTreeSelection(rows.length - 1)");
    expect(handler).toContain("row.depth + 1");
    expect(handler).toContain("parentRowIndex(selectedIndex)");
    expect(source).toContain('.map((row) => ({ ...row, depth: 0 }))');
  });

  it("connects container focus to the active virtual tree item", () => {
    expect(source).toContain("aria-activedescendant=");
    expect(source).toContain("function treeItemId");
    expect(source).toContain("containerEl?.clientHeight");
    expect(source).toContain("itemBottom > scrollTop + viewportHeight");
    expect(source).not.toContain("containerEl.scrollTo");
    expect(source).toContain("aria-level={r.depth + 1}");
    expect(source).toContain('aria-expanded={r.kind === "dir" ? !isCollapsed(r.path) : undefined}');
  });

  it("opens and operates the context menu from the standard keyboard keys", () => {
    expect(source).toContain('e.key === "ContextMenu"');
    expect(source).toContain('e.key === "F10" && e.shiftKey');
    expect(source).toContain("function handleMenuKeydown");
    for (const key of ['"Escape"', '"Tab"', '"ArrowDown"', '"ArrowUp"', '"Home"', '"End"']) {
      expect(source).toContain(key);
    }
    expect(source).toContain("menuEl?.querySelector<HTMLElement>");
    expect(source).toContain("closeContextMenu({ restoreFocus: true })");
  });

  it("lets Tab and Shift+Tab leave the context menu in document order", () => {
    const handler = source.slice(
      source.indexOf("function handleMenuKeydown"),
      source.indexOf("async function copyPath"),
    );
    const tabBranch = handler.slice(
      handler.indexOf('e.key === "Tab"'),
      handler.indexOf('e.key === "Escape"'),
    );

    expect(tabBranch).toContain("focusAdjacentToMenuOpener(");
    expect(tabBranch).toContain("e.shiftKey");
    expect(tabBranch).not.toContain("restoreFocus");
    expect(source).toContain("candidate.tabIndex >= 0");
  });

  it("uses a real labelled action button and clamps the popup to the viewport", () => {
    expect(source).toContain('aria-label={`Actions for ${r.path}`}');
    expect(source).toContain("clampMenuPosition(");
    expect(source).not.toContain('<span\n                    role="button"');
  });

  it("reports path-copy failure through the shared clipboard seam", () => {
    expect(source).toContain("async function copyPath");
    expect(source).toContain("if (!(await copyText(path)))");
    expect(source).toContain('repoStore.setError("Could not copy path to clipboard")');
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("filters with glob, regex, and fuzzy queries and reloads when status membership changes", () => {
    expect(source).toContain("parseFileQuery");
    expect(source).toContain("filterPathsByFileQuery");
    expect(source).toContain("statusPathKey");
    expect(source).toContain("joinWorktreePath");
    expect(source).toContain("onPinFile");
  });

  it("supports zero-config store fallbacks for shared explorer selection", () => {
    expect(source).toContain("onSelectFile?: (path: string) => void");
    expect(source).toContain("repoStore.selectFilePath(path)");
    expect(source).toContain("effectiveSelected = $derived(selectedFile ?? $repoStore.selectedFilePath)");
  });

  it("builds rows via pure fileTree pipeline and reveals active file ancestors", () => {
    expect(source).toContain('from "../../files/fileTree"');
    for (const fn of ["buildFileTree", "flattenFileTree", "ancestorsOf"]) {
      expect(source).toContain(fn);
    }
    expect(source).toContain("isFiltering ? false : collapsed[dirPath] === true");
    expect(source).toContain("ancestorsOf(selected)");
    expect(source).toContain("if (changed) collapsed = next;");
  });
});
