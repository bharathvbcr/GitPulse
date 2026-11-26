import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "../FileViewer.svelte"), "utf8");
const storeSource = readFileSync(join(here, "../../files/openDocuments.ts"), "utf8");

describe("FileViewer", () => {
  it("imports and orchestrates the Explorer rail, MediaViewer, and LivePulseDashboard", () => {
    // The rail holds the file tree and content search (ExplorerRail.svelte).
    expect(source).toContain("<ExplorerRail");
    expect(source).toContain("MediaViewer");
    expect(source).toContain("LivePulseDashboard");
  });

  it("fetches Blame, the editor's other lens, only when it is first toggled on", () => {
    // Blame was its own lazily loaded section. As a toggle it renders inside
    // this component, and a static import would pull the whole blame view and
    // its timeline into the Code chunk for a reader who never asks for it.
    expect(source).not.toMatch(/^\s*import\s+BlameViewer\b/m);
    expect(source).toContain('const loadBlame = () => import("./BlameViewer.svelte");');
    expect(source).toContain("<LazyView load={loadBlame}");
  });

  it("reads the open file through the open-file store, which guards every read", () => {
    // The read, its stale-answer guard and the FileBlob type live in the
    // store (see openDocuments.test.ts for the behaviour); the page only asks.
    expect(source).toContain("loadDocument(repo, path, { force: true })");
    expect(source).not.toContain("cmd_get_file_blob");
    expect(storeSource).toContain('invoke<FileBlob>("cmd_get_file_blob"');
    expect(storeSource).toContain("latestRead.get(key) !== token");
    expect(storeSource).toContain('import type { FileBlob, SavedFile } from "./types"');
  });

  it("sends overwrite only from the banner's Overwrite, so no ordinary save can skip the encoder", () => {
    expect(source).toContain("saveDraft(path, draft.content, conflict.actual, true)");
    expect(storeSource).toContain("...(options.overwrite ? { overwrite: true } : {})");
    expect(source).toContain("overwriteNote={overwriteNote(activeBlob)}");
    expect(source).not.toMatch(/onSave=\{[^}]*true\)/);
  });

  it("keeps editor tabs across view remounts with preview vs pin semantics", () => {
    expect(source).toContain("createRepoPanelCache");
    expect(source).toContain("openPreview");
    expect(source).toContain("openPinned");
    expect(source).toContain("activateEditorTab");
    expect(source).toContain("onPinFile");
    expect(source).toContain("closeTab");
    expect(source).toContain("closeAllTabs");
    expect(source).toContain("closeOtherTabs");
    expect(source).toContain("pathSegments");
  });

  it("reads per-path drafts from the open-file store and keeps a repository's tabs while it owes one", () => {
    expect(source).toContain("documentIn($openDocuments, $currentRepoPath, activeTabPath)");
    expect(source).toContain("let activeDraft = $derived(activeDoc?.draft ?? undefined)");
    expect(source).toContain("canEvict: (_cached, repo) => draftPaths(repo).length === 0");
    expect(source).toContain("draftContent={activeDraft?.content ?? null}");
    expect(source).toContain("onDraftChange=");
    // The tabs no longer carry drafts: there is one copy, and it is the store's.
    expect(source).not.toContain("drafts:");
    expect(source).not.toContain("tabState.drafts");
  });

  it("claims its open tabs in the store on every change and on hydration, shown or not", () => {
    const persist = source.slice(source.indexOf("function persistTabs"), source.indexOf("function previewFile"));
    expect(persist).toContain("claimDocuments(repo, HOLDER, tabState.tabs.map((tab) => tab.path))");
    const hydrate = source.slice(source.indexOf("let cached = tabCache.get(repo)"), source.indexOf("const storeSelection"));
    expect(hydrate).toContain("claimDocuments(repo, HOLDER, tabState.tabs.map((tab) => tab.path))");
  });

  it("leaves the app-close registry to the store, the one owner of editor drafts", () => {
    // Two owners publishing the same file is how the quit guard could count it
    // twice; the store publishes after every change to a draft.
    expect(source).not.toContain("recordEditorDrafts");
    expect(storeSource).toContain('recordEditorDrafts(repo, paths, "documents")');
    expect(storeSource).toContain("if (next.draft !== doc.draft) publishDrafts(repo)");
  });

  it("asks before close operations only when they remove dirty drafts", () => {
    expect(source).toContain("await fileSaveQueue.whenIdle(saveKeys)");
    expect(source).toContain("dirtyEditorTabPaths(");
    expect(source).toContain("hasDraft(repo, path) && !(closing && isHeldElsewhere(repo, path, HOLDER))");
    expect(source).toContain("if (dirtyPaths.length === 0) return [];");
    expect(source).toContain("const confirmed = await askConfirm({");
    expect(source).toContain("await closeTabsAfterConfirm([path])");
    expect(source).toContain("await closeTabsAfterConfirm(pathsAtRequest)");
    expect(source).toContain("await closeTabsAfterConfirm(pathsToClose)");
  });

  it("applies delayed bulk closes only to the tabs captured by that request", () => {
    const closeAll = source.slice(
      source.indexOf("async function closeAllTabs"),
      source.indexOf("async function closeOtherTabs"),
    );
    expect(closeAll).toContain("const pathsAtRequest = tabState.tabs.map");
    expect(closeAll).toContain("await closeTabsAfterConfirm(pathsAtRequest)");
    expect(closeAll).not.toContain("closeAllEditorTabs()");

    const closer = source.slice(
      source.indexOf("async function closeTabsAfterConfirm"),
      source.indexOf("function isCurrentViewer"),
    );
    expect(closer).toContain("closeEditorTabs(tabState, paths)");
    // Drafts go only after the user said so, and only the ones this close removes.
    expect(closer.indexOf("confirmDiscardDrafts(paths, true)")).toBeLessThan(closer.indexOf("discardDraft(repo, path)"));

    const closeOthers = source.slice(
      source.indexOf("async function closeOtherTabs"),
      source.indexOf("function handleDraftChange"),
    );
    expect(closeOthers).toContain("const keepPath = tabState.active");
    expect(closeOthers).toContain("closeTabsAfterConfirm(pathsToClose)");
    expect(closeOthers).not.toContain("closeOtherEditorTabs(tabState");
  });

  it("clears a draft only after its write succeeds", () => {
    const save = storeSource.slice(
      storeSource.indexOf("export async function saveDocument"),
      storeSource.indexOf("function refuse"),
    );
    const write = save.indexOf('await invoke<Guarded<SavedFile>>("cmd_write_file_content"');
    const complete = save.indexOf("completeSave(repo, target");
    expect(write).toBeGreaterThan(-1);
    expect(complete).toBeGreaterThan(write);
    // The catch only records a refusal; it never completes (clears) a draft.
    expect(save.slice(save.indexOf("} catch"))).not.toContain("completeSave(");
  });

  it("serializes the complete save lifecycle by canonical repository and path", () => {
    const save = storeSource.slice(
      storeSource.indexOf("export async function saveDocument"),
      storeSource.indexOf("function refuse"),
    );
    const queued = save.indexOf("editorFileSaveQueue.run(fileSaveKey(repo, target)");
    const write = save.indexOf('invoke<Guarded<SavedFile>>("cmd_write_file_content"');
    const complete = save.indexOf("completeSave(repo, target, content, saved.output.disk_base)");
    const after = save.indexOf("await options.afterSave?.()");
    expect(queued).toBeGreaterThan(-1);
    expect(write).toBeGreaterThan(queued);
    expect(complete).toBeGreaterThan(write);
    expect(after).toBeGreaterThan(complete);
    // The page's refresh runs as that hook, so it stays ordered with the write.
    expect(source).toContain("afterSave: async () => {");
    expect(source).toContain("await repoStore.refresh()");
  });

  it("has no per-viewer copy of a draft to complete: a save lands on the one document", () => {
    // It used to complete against the tab cache and dispatch to the newest
    // viewer so a remounted page saw the result. The document is global, so a
    // viewer of any age reads the completed state (openDocuments.test.ts:
    // "keeps text typed while a write was in flight ...").
    expect(source).not.toContain("completeCachedEditorSave");
    expect(source).not.toContain("applyCompletedSaveToCurrentViewer");
    expect(source).not.toContain("completeEditorSave");
    expect(source).toContain("const activeDoc = $derived(documentIn(");
  });

  it("binds writes to the repository that owns the displayed editor", () => {
    const save = source.slice(
      source.indexOf("async function saveDraft"),
      source.indexOf("/** Overwrite: save the draft"),
    );
    expect(save).toContain("const repo = hydratedRepo;");
    expect(save).toContain("$repoStore.currentPath !== repo");
    // Checked before the queue is entered and again inside it, just before the write.
    expect(save.indexOf("repositoryChanged();")).toBeLessThan(save.indexOf("await saveDocument("));
    expect(save).toContain("beforeWrite: repositoryChanged");
  });

  it("shows dirty state and forwards explicit draft/discard ownership", () => {
    expect(source).toContain("isTabDirty(tab.path)");
    expect(source).toContain("Unsaved changes");
    expect(source).toContain("onRequestDiscard=");
  });

  it("does not apply a pending discard decision after the repository changes", () => {
    for (const fn of ["async function confirmDiscardDrafts", "async function closeTabsAfterConfirm", "async function requestDiscardDraft"]) {
      const start = source.indexOf(fn);
      const body = source.slice(start, source.indexOf("\n  }\n", start));
      expect(body, fn).toContain("!isCurrentViewer(repo)");
    }
    // Only the viewer that asked may discard after the answer: the discards
    // follow the check, not the confirmation.
    const closer = source.slice(source.indexOf("async function closeTabsAfterConfirm"));
    expect(closer.indexOf("!isCurrentViewer(repo)")).toBeLessThan(closer.indexOf("discardDraft(repo, path)"));
  });

  it("synchronizes the shared file selection after every tab-closing operation", () => {
    expect(source).toContain("function syncSelectedFilePath()");
    expect(source).toContain("repoStore.selectFilePath(tabState.active)");
    const closeTabBody = source.slice(
      source.indexOf("async function closeTab"),
      source.indexOf("async function closeAllTabs"),
    );
    const closeAllBody = source.slice(
      source.indexOf("async function closeAllTabs"),
      source.indexOf("async function closeOtherTabs"),
    );
    const closeOthersBody = source.slice(
      source.indexOf("async function closeOtherTabs"),
      source.indexOf("function handleDraftChange"),
    );
    expect(closeTabBody).toContain("syncSelectedFilePath()");
    expect(closeAllBody).toContain("syncSelectedFilePath()");
    expect(closeOthersBody).toContain("syncSelectedFilePath()");
  });

  it("toggles explorer and dashboard from the keyboard without stealing Cmd+W", () => {
    expect(source).toContain("explorerOpen");
    expect(source).toContain("dashboardOpen");
    expect(source).toContain('"b"');
    expect(source).toContain('"d"');
    expect(source).not.toContain('"w"');
  });

  it("reads a file's history through one owner that keeps a failed read distinct from none", () => {
    const pulse = readFileSync(join(here, "LivePulseDashboard.svelte"), "utf8");
    const menu = readFileSync(join(here, "FileHistoryMenu.svelte"), "utf8");
    for (const surface of [pulse, menu]) {
      expect(surface).toContain("loadFileHistory(invoke,");
      expect(surface).not.toContain("query: `path:");
    }
    // Live pulse states a failed or unsupported read before it can say "no commits".
    expect(pulse.indexOf("{:else if historyNote}")).toBeGreaterThan(-1);
    expect(pulse.indexOf("{:else if historyNote}")).toBeLessThan(pulse.indexOf("No prior commits recorded"));
    // The open file's path bar carries the History menu beside Diff.
    expect(source).toContain("<FileHistoryMenu repoPath={$currentRepoPath} path={activeTabPath} />");
    expect(menu).toContain("repoStore.selectCommitFileDiff(commitId, path)");
  });

  it("ignores its pane shortcuts while covered (inert) or already handled", () => {
    const body = source.slice(source.indexOf("function handleWindowKeydown"), source.indexOf("$effect(", source.indexOf("function handleWindowKeydown")));
    const guard = body.indexOf('closest("[inert]")');
    expect(guard).toBeGreaterThan(-1);
    expect(body).toContain("e.defaultPrevented");
    // The guard runs before either toggle.
    expect(guard).toBeLessThan(body.indexOf('toggleSidePane("explorer")'));
    expect(guard).toBeLessThan(body.indexOf('toggleSidePane("dashboard")'));
  });

  it("keeps Live pulse's visibility in one remembered preference, not per repository", () => {
    expect(source).toContain("const dashboardOpen = $derived($livePulsePref.visible)");
    expect(source).not.toMatch(/dashboardOpen\s*=\s*(true|false|cached)/);
    const cacheType = source.slice(source.indexOf("createRepoPanelCache<{"), source.indexOf("}>(", source.indexOf("createRepoPanelCache<{")));
    expect(cacheType).not.toContain("dashboardOpen");
    // The card's Hide is an explicit choice: remembered even on a compact
    // layout, where the toolbar toggle only ends a transient peek.
    expect(source).toContain("onHide={hideLivePulse}");
    const hide = source.slice(source.indexOf("function hideLivePulse"), source.indexOf("}", source.indexOf("function hideLivePulse")));
    expect(hide).toContain("livePulsePref.setVisible(false)");
    expect(hide).toContain("livePulseToggle?.focus()");
  });

  it("arbitrates fixed side panes from the measured File view width", () => {
    expect(source).toContain("bind:this={fileViewRoot}");
    expect(source).toContain("observeResize");
    expect(source).toContain("resolveFilePaneLayout");
    expect(source).toContain("{#if paneLayout.explorerVisible}");
    expect(source).toContain("{#if paneLayout.editorVisible}");
    expect(source).toContain("{#if paneLayout.dashboardVisible}");
  });

  it("draws the Code page's bar on the page's ground, with no band fill or edge line", () => {
    const bar = source.slice(source.lastIndexOf("<div", source.indexOf("data-file-view-bar")), source.indexOf(">", source.indexOf("data-file-view-bar")));
    expect(bar).not.toMatch(/bg-surface|gp-section-edge/);
    const pulse = readFileSync(join(here, "LivePulseDashboard.svelte"), "utf8");
    const head = pulse.slice(pulse.indexOf('<div class="flex items-center justify-between px-3 py-2.5'), pulse.indexOf(">", pulse.indexOf('<div class="flex items-center justify-between px-3 py-2.5')));
    expect(head).not.toMatch(/bg-surface|gp-section-edge/);
  });

  it("gives the Live pulse card the same clear ground as the Explorer card beside it", () => {
    const pulse = readFileSync(join(here, "LivePulseDashboard.svelte"), "utf8");
    const body = pulse.slice(pulse.indexOf("</script>"));
    const root = body.slice(body.indexOf("<div"), body.indexOf(">", body.indexOf("<div")) + 1);
    expect(root).toContain("flex flex-col h-full");
    expect(root).not.toMatch(/\bbg-surface(\/\d+)?\b/);
  });

  it("draws the Live pulse column as a floating card that stretches with its row", () => {
    const host = source.slice(source.indexOf("{#if paneLayout.dashboardVisible}"), source.indexOf("<LivePulseDashboard"));
    expect(host).toContain("gp-float-card");
    expect(host).toContain("data-live-pulse-card");
    // The card's margin plus `h-full` would overflow the row; it stretches instead.
    expect(host).not.toContain("h-full");
    const css = readFileSync(new URL("../../../app.css", import.meta.url), "utf8");
    const card = css.slice(css.indexOf("@utility gp-float-card {"), css.indexOf("}", css.indexOf("@utility gp-float-card {")));
    expect(card).toContain("margin: var(--gp-pane-gutter, 6px);");
    expect(card).toContain("border-radius: var(--gp-r-pane);");
    expect(card).toContain("box-shadow: var(--shadow-card);");
    expect(card).toContain("overflow: hidden;");
  });

  it("keeps the preferred side pane repository-scoped across view remounts", () => {
    expect(source).toContain("preferredSidePane: FileSidePane;");
    expect(source).toContain("preferredSidePane,\n    });");
    expect(source).toContain("preferredSidePane = cached.preferredSidePane ?? \"explorer\";");
    expect(source).toContain("preferredSidePane = \"explorer\";");
  });

  it("writes file content via the store's cmd_write_file_content and opens through the shell gate", () => {
    expect(storeSource).toContain('invoke<Guarded<SavedFile>>("cmd_write_file_content"');
    expect(source).toContain("saveDocument(repo, path, newContent");
    expect(source).toContain("openInDefaultApp");
    expect(source).not.toContain("@tauri-apps/plugin-opener");
    expect(source).toContain("describeError");
  });

  it("sends the draft's base with every save and re-bases the document on the saved one", () => {
    const save = storeSource.slice(
      storeSource.indexOf("export async function saveDocument"),
      storeSource.indexOf("function refuse"),
    );
    expect(save).toContain("expectedBase: judged.base");
    expect(save).toContain("saveBase(doc?.draft ?? undefined, doc?.blob?.disk_base ?? null, options.overrideBase)");
    expect(save).toContain('judged === "unbased"');
    expect(storeSource).toContain("disk_base: savedBase");
    // The first dirtying keystroke is what records the base.
    expect(source).toContain("editDocument(repo, path, newContent, sourceContent)");
    expect(storeSource).toContain("base: existing ? existing.base : doc.blob?.disk_base ?? null");
  });

  it("answers a changed_on_disk refusal with a kept draft and Reload / Overwrite / Compare", () => {
    const save = storeSource.slice(
      storeSource.indexOf("export async function saveDocument"),
      storeSource.indexOf("function refuse"),
    );
    expect(save).toContain('typed?.code === "changed_on_disk"');
    expect(save).toContain("refuse(repo, target, typed.actual)");
    expect(save).toContain("void loadDocument(repo, target, { force: true })");
    // The error is rethrown so the editor stays in edit mode and the draft stays dirty.
    expect(save).toContain("throw err;");
    const page = source.slice(source.indexOf("async function overwriteDraft"), source.indexOf("async function requestDiscardDraft"));
    expect(page).toContain("saveDraft(path, draft.content, conflict.actual, true)");
    expect(page).toContain("requestDiscardDraft(path)");
    expect(page.indexOf("requestDiscardDraft(path)")).toBeLessThan(
      page.indexOf("await loadDocument(repo, path, { force: true, remount: true })"),
    );
    expect(source).toContain("<EditorConflictBanner");
    expect(source).toContain("onReload=");
    expect(source).toContain("onOverwrite=");
    expect(source).toContain("onSave={(newContent) => saveDraft(activeTabPath, newContent)}");
  });

  it("marks a draft conflicted as soon as the reloaded blob's base differs", () => {
    expect(source).toContain("documentConflict(activeDoc)");
    expect(storeSource).toContain("diskBase != null && diskBase !== doc.draft.base");
  });

  it("surfaces clipboard failure when copying the active path", () => {
    expect(source).toContain("if (!(await copyText(activeTabPath)))");
    expect(source).toContain(`repoStore.setError("Couldn't copy path to clipboard")`);
  });

  it("renders LanguageLogo for open editor tabs", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("filePath={tab.path}");
  });

  it("scrolls a long path instead of letting segments cover the path actions", () => {
    const path = source.slice(source.indexOf("bind:this={pathStrip}"));
    expect(path).toContain("gp-header-scroll");
    expect(path).toContain("min-w-max");
    expect(path).toContain('target={pathStrip} axis="x"');
    expect(path).not.toContain("max-w-[14ch]");
  });

  it("sizes the explorer from the shared file-tree column instead of a fixed w-72", () => {
    // The column used to be `w-72 shrink-0` (288px) with no handle. Blame
    // renders beside it in the editor pane, so this is the only tree width.
    const row = source.slice(source.indexOf("flex-1 flex min-h-0 overflow-hidden relative"));
    expect(row).toContain("<FileTreeColumn");
    expect(row).toContain("reserve={explorerReserve}");
    expect(row).not.toContain("w-72");
    expect(source).toContain('import FileTreeColumn from "./files/FileTreeColumn.svelte"');
  });

  it("auto-hides its duplicate ExplorerRail while the docked main Sidebar is already showing Explorer, unless explicitly opened", () => {
    expect(source).toContain("sidebarExplorerDocked");
    expect(source).toContain("explorerForcedWithOuterSidebar");
    expect(source).toContain("explorerRequested: effectiveExplorerOpen");
  });

  it("exits Blame lens once per HTML preview or split open request nonce without locking Blame out", () => {
    expect(source).toContain("htmlViewModeRequest");
    expect(source).toContain("matchHtmlViewMode(req, repo, path)");
    expect(source).toContain("matched.nonce > lastHandledHtmlModeNonce");
    expect(source).toContain('setEditorLens("edit")');
  });

  it("exits Blame lens once per SVG preview or split open request nonce without locking Blame out", () => {
    expect(source).toContain("svgViewModeRequest");
    expect(source).toContain("matchSvgViewMode(req, repo, path)");
    expect(source).toContain("matched.nonce > lastHandledSvgModeNonce");
  });
});
