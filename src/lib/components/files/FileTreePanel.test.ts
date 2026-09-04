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

  /**
   * The five scopes used to be five hand-written buttons, and this asserted
   * the five string literals those buttons happened to contain — which meant
   * it tracked quoting rather than behaviour. They are one table now, so the
   * check is that the table and the type agree and that the table is what the
   * markup renders: a scope added to `StatusFilter` and forgotten in
   * `STATUS_SCOPES` is unreachable, and that is the failure worth catching.
   */
  it("offers every declared status scope as a filter", () => {
    const union = source.slice(source.indexOf("type StatusFilter ="));
    const declared = (union.slice(0, union.indexOf(";")).match(/"([a-z]+)"/g) ?? []).map((raw) =>
      raw.replaceAll('"', ""),
    );
    expect(declared).toEqual(["all", "modified", "staged", "untracked", "conflicted"]);

    const table = source.slice(source.indexOf("const STATUS_SCOPES"), source.indexOf("] as const"));
    for (const scope of declared.filter((value) => value !== "all")) {
      expect(table, `${scope} is missing from STATUS_SCOPES`).toContain(`id: "${scope}"`);
    }
    expect(source).toContain("{#each STATUS_SCOPES as scope");
    expect(source).toContain('statusFilter = statusFilter === scope.id ? "all" : scope.id');
    expect(source).toContain('onclick={() => (statusFilter = "all")}');
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

  /**
   * The reveal effect reads and writes `collapsed`. Tracked, that made every
   * expand and collapse re-arm `locatePath` and scroll the tree back to the
   * open file — so opening a folder in a large repository discarded the
   * position the user had just navigated to. Only a new selection reveals.
   */
  it("does not re-reveal the open file when a folder is expanded or collapsed", () => {
    const effect = source.slice(
      source.indexOf("$effect(() => {\n    const selected = effectiveSelected;"),
      source.indexOf("let lastRevealNonce"),
    );
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("locatePath = selected"));
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("{ ...collapsed }"));
    expect(source).toContain('import { onMount, untrack } from "svelte"');
  });

  /**
   * The breadcrumb's folder crumbs raise these. The nonce is the contract: two
   * clicks on one crumb are two requests, and comparing paths alone would
   * swallow the second after the user had scrolled away.
   */
  it("reveals a requested directory once per request, by nonce", () => {
    expect(source).toContain("revealRequest?: { path: string; nonce: number } | null");
    const effect = source.slice(
      source.indexOf("let lastRevealNonce"),
      source.indexOf("$effect(() => {\n    if (selectedIndex >= rows.length)"),
    );
    expect(effect).toContain("request.nonce === lastRevealNonce");
    expect(effect).toContain("lastRevealNonce = request.nonce;");
    // The folder itself opens too, not only the path down to it.
    expect(effect).toContain("[...ancestorsOf(request.path), request.path]");
    expect(effect).toContain("locatePath = request.path;");
    // And the row lookup must accept a directory, not just a file.
    expect(source).toContain("const idx = rows.findIndex((row) => row.path === target);");
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
