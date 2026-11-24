import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { functionBody } from "../../stores/sourceScan";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./FileTreePanel.svelte", import.meta.url), "utf8");

/**
 * What the explorer does is checked by rendering it: `harness/file-tree.html`
 * (`node scripts/browser-regressions.mjs --harness file-tree`) drives the real
 * panel for the single-select tree keys and the active item a screen reader
 * follows, the glob filter and status scopes, following the open file, the
 * context menu's keys and focus return, the delete and discard confirmations,
 * create, rename and move, which index action each file is offered, the
 * unverified-churn mark, the filter-budget caption, and long-name scrolling.
 * `harness/repo-churn.html` checks that a publish does not re-list the
 * repository. Only what a rendered page cannot see stays here.
 */
describe("FileTreePanel source contracts", () => {
  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  /**
   * Opening and revealing go through `desktop/openInShell`, not the opener
   * plugin. The plugin's commands take an absolute path from the webview,
   * which made the worktree containment check optional; these hand Rust the
   * root and the relative path separately so it can prove containment. The
   * repo-wide version of this assertion lives in `openInShell.test.ts`.
   */
  it("opens and reveals through the validated shell gate, not the opener plugin", () => {
    expect(source).toContain("openInDefaultApp");
    expect(source).toContain("revealInFileManager");
    expect(source).not.toContain("@tauri-apps/plugin-opener");
  });

  /**
   * The external editor goes through the preference helper and Rust's gate,
   * and the in-app viewer keeps its own, differently named, row: two rows both
   * called "Open in editor" would be one the user cannot tell apart.
   */
  it("offers the external editor beside the in-app viewer under distinct names", () => {
    expect(source).toContain("openInPreferredEditor(repo, filePath)");
    expect(source).toContain("reportEditorFailure(err)");
    expect(source.match(/<span>Open in editor<\/span>/g)).toHaveLength(1);
    expect(source.match(/<span>Open in viewer<\/span>/g)).toHaveLength(1);
  });

  /**
   * New File and New Folder's `.gitkeep` used to go through the editor's
   * overwrite command with empty content, which truncated an existing file.
   * The rendered behaviour (existing name refused, nothing written) is the
   * file-tree browser harness; this keeps the overwrite command out of the
   * explorer.
   */
  it("creates through the create-only command and never the overwrite one", () => {
    // The store owns the command; the panel asks it to create and reads the
    // typed refusal off the outcome.
    const store = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "stores", "repoStore.ts"), "utf8");
    expect(store).toContain('"cmd_create_file"');
    expect(store).not.toContain("cmd_write_file_content");
    expect(source).toContain("repoStore.createFile(path, target)");
    expect(source).not.toContain("cmd_create_file");
    expect(source).not.toContain("cmd_write_file_content");
    expect(source).toContain("validate: (value) => newPathProblem(");
    expect(source).toContain('typedError?.code === "already_exists"');
  });

  it("discards against the repository it asked about, and keeps every index outcome", () => {
    const discard = functionBody(source, "discardFile");
    expect(discard.indexOf("repoStore.captureTarget()")).toBeGreaterThan(-1);
    expect(discard.indexOf("repoStore.captureTarget()")).toBeLessThan(discard.indexOf("await askConfirm("));
    expect(discard).toContain("repoStore.discardChanges(path, target)");
    expect(discard).toContain("reportFailure(");
    expect(functionBody(source, "stageFile")).toContain("return await repoStore.stageFile(path);");
    expect(functionBody(source, "unstageFile")).toContain("return await repoStore.unstageFile(path);");
  });

  /**
   * The filter sits in a rounded-full pill, and the pill's edge is the focus
   * chrome. A rectangular ring inside it is what this guards against: `app.css`
   * gives a typed text box no ring (owner decision 2026-11-11), so the input
   * needs no cancelling rule of its own, and must not have one: an unlayered
   * `outline: none` on it would also remove the forced-colours outline, the
   * floor that keeps keyboard focus visible in high contrast.
   */
  it("keeps filter focus on the rounded pill, with no rectangular ring and the forced-colours floor intact", () => {
    const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");
    const ringRuleStart = css.indexOf("button:focus-visible");
    expect(ringRuleStart).toBeGreaterThan(-1);
    const ringRule = css.slice(ringRuleStart, css.indexOf("}", ringRuleStart) + 1);
    expect(ringRule).toContain("box-shadow: var(--ring-focus)");
    // The ring goes to controls you operate, never to a text box you type in.
    expect(ringRule).not.toMatch(/(^|[\s,])input:focus-visible/);
    const textRuleStart = css.indexOf("input:not([type=\"checkbox\"]");
    expect(textRuleStart).toBeGreaterThan(-1);
    const textRule = css.slice(textRuleStart, css.indexOf("}", textRuleStart) + 1);
    expect(textRule).not.toContain("box-shadow");
    const forcedAt = css.indexOf("@media (forced-colors: active)");
    expect(forcedAt).toBeGreaterThan(-1);
    const forced = css.slice(forcedAt, css.indexOf("}", css.indexOf("{", css.indexOf("{", forcedAt) + 1)) + 1);
    expect(forced).toContain("input:focus-visible");
    expect(forced).toContain("outline: 2px solid Highlight");

    // The one Explorer search field: its placeholder follows the mode.
    const marker = "placeholder={fieldLabel}";
    const at = source.indexOf(marker);
    expect(at).toBeGreaterThan(-1);
    const pillAt = source.lastIndexOf("file-tree-filter-pill", at);
    const field = source.slice(source.lastIndexOf("<div", pillAt), source.indexOf("/>", at));
    expect(field).toContain("file-tree-filter-pill");
    expect(field).toContain("rounded-full");
    // The field focus edge every text field uses, on the pill.
    expect(field).toContain("focus-within:border-(--field-focus-edge)");
    expect(field).not.toContain("focus-within:shadow-(--ring-focus)");
    expect(field).not.toContain("focus:outline-hidden");

    // Nothing scoped re-adds a ring or takes the forced-colours outline away.
    const styleAt = source.indexOf("<style>");
    const style = styleAt === -1 ? "" : source.slice(styleAt, source.indexOf("</style>", styleAt));
    expect(style).not.toContain("var(--ring-focus)");
    expect(style).not.toMatch(/\.file-tree-filter:focus/);
  });

  /**
   * One search field, not two: Explorer's toolbar carries no separate content
   * search, and the field's own contents toggle is the only way into Contents
   * besides the palette. Names is the default; the toggle is one icon, not a
   * Names | Contents segment, because the segment could not sit beside the
   * text in the 264px sidebar and wrapped the pill onto a second row.
   */
  it("is the single search field, with one contents toggle inside it", () => {
    expect(source.match(/<input\b/g)?.length).toBe(1);
    const pillAt = source.indexOf("file-tree-filter-pill");
    const toggleAt = source.indexOf("data-explorer-contents-toggle", pillAt);
    expect(toggleAt).toBeGreaterThan(pillAt);
    const toggle = source.slice(source.lastIndexOf("<button", toggleAt), source.indexOf("</button>", toggleAt));
    expect(toggle).toContain("aria-pressed={contentMode}");
    expect(toggle).toContain("toggledSearchMode(mode)");
    expect(source).not.toContain("<SettingSegment");
    // The pill never wraps: no flex-wrap and no narrow-width second row.
    const pill = source.slice(source.lastIndexOf("<div", pillAt), source.indexOf(">", pillAt));
    expect(pill).not.toContain("flex-wrap");
    expect(pill).not.toContain("@max-");

    const viewer = readFileSync(new URL("../FileViewer.svelte", import.meta.url), "utf8");
    expect(viewer).not.toContain("data-search-toggle");
    expect(viewer).not.toContain("toggleSearchRail");
  });

  it("hides the tree in Contents mode instead of unmounting it", () => {
    expect(source).toContain("class:hidden={contentMode}");
    expect(source).toContain("class:hidden={!contentMode}");
    expect(source).not.toMatch(/\{#if contentMode\}\s*<div[^>]*role="tree"/);
  });

  it("sends Enter in Contents mode to the content search rather than the tree", () => {
    const body = functionBody(source, "handleFieldKeydown");
    expect(body).toContain("onContentSubmit");
    expect(body).toContain("fieldEscape(");
  });

  it("uses effectiveRepoPath for drag payloads, retry buttons, and copy full path so docked worktree explorers never leak currentRepoPath", () => {
    const dragAt = source.indexOf("ondragstart={(e) => {");
    expect(dragAt).toBeGreaterThan(-1);
    const dragBlock = source.slice(dragAt, source.indexOf("ondragend=", dragAt));
    expect(dragBlock).toContain("effectiveRepoPath");
    expect(dragBlock).not.toContain("$currentRepoPath");
    expect(source).not.toContain("if ($currentRepoPath) void loadFiles($currentRepoPath, true)");
    expect(source).not.toContain("copyPath(`${$repoStore.currentPath}/${row.path}`)");
  });

  it("offers Open as Preview and Open Code & Preview in the right-click context menu for HTML files", () => {
    expect(source).toContain("isHtmlFile(row.path)");
    expect(source).toContain('data-testid="file-ctx-html-preview"');
    expect(source).toContain('data-testid="file-ctx-html-split"');
    expect(source).toContain('data-testid="file-ctx-html-code"');
    expect(source).toContain("Open as Preview");
    expect(source).toContain("Open Code &amp; Preview");
    const helper = functionBody(source, "openHtmlInMode");
    expect(helper).toContain("requestHtmlViewMode(effectiveRepoPath, path, mode)");
    expect(helper).toContain("onSelectFile(path)");
  });

  it("offers Open as Preview and Open Code & Preview in the right-click context menu for SVG files", () => {
    expect(source).toContain("isSvgFile(row.path)");
    expect(source).toContain('data-testid="file-ctx-svg-preview"');
    expect(source).toContain('data-testid="file-ctx-svg-split"');
    expect(source).toContain('data-testid="file-ctx-svg-code"');
    const helper = functionBody(source, "openSvgInMode");
    expect(helper).toContain("requestSvgViewMode(effectiveRepoPath, path, mode)");
    expect(helper).toContain("onSelectFile(path)");
  });
});
