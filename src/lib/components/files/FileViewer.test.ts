import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "../FileViewer.svelte"), "utf8");

describe("FileViewer", () => {
  it("imports and orchestrates FileTreePanel, MediaViewer, and LivePulseDashboard", () => {
    expect(source).toContain("FileTreePanel");
    expect(source).toContain("MediaViewer");
    expect(source).toContain("LivePulseDashboard");
  });

  it("loads file content via cmd_get_file_blob with async protection", () => {
    // FileBlobPayload was a component-local copy of the canonical FileBlob,
    // field-for-field identical and unable to fail on drift. Asserting the
    // shared type keeps a local re-declaration from creeping back.
    expect(source).toContain('invoke<FileBlob>("cmd_get_file_blob"');
    expect(source).toContain('import type { FileBlob } from "../files/types"');
    expect(source).toContain("createAsyncGuard");
    expect(source).toContain("guard.isLive()");
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

  it("owns per-path drafts inside the per-repository tab cache", () => {
    expect(source).toContain("updateEditorDraft");
    expect(source).toContain("editorDraft(tabState, activeTabPath)");
    expect(source).toContain("hasDirtyEditorTabs");
    expect(source).toContain("canEvict:");
    expect(source).toContain("draftContent={activeDraft?.content ?? null}");
    expect(source).toContain("onDraftChange=");
  });

  it("publishes every cached draft transition to the app-close registry", () => {
    const persist = source.slice(
      source.indexOf("function persistTabs"),
      source.indexOf("async function loadFileContent"),
    );
    expect(source).toContain('import { recordEditorDrafts } from "../files/editorDraftRegistry"');
    expect(persist).toContain("tabCache.set(repo");
    expect(persist).toContain("recordEditorDrafts(repo, Object.keys(tabState.drafts))");
    expect(persist.indexOf("recordEditorDrafts")).toBeGreaterThan(
      persist.indexOf("tabCache.set"),
    );
  });

  it("asks before close operations only when they remove dirty drafts", () => {
    expect(source).toContain("await fileSaveQueue.whenIdle(saveKeys)");
    expect(source).toContain("dirtyEditorTabPaths(tabState, candidatePaths)");
    expect(source).toContain("if (dirtyPaths.length === 0) return true;");
    expect(source).toContain("return askConfirm({");
    expect(source).toContain("await confirmDiscardDrafts([path])");
    expect(source).toContain("await confirmDiscardDrafts(pathsAtRequest)");
    expect(source).toContain("await confirmDiscardDrafts(pathsToClose)");
  });

  it("applies delayed bulk closes only to the tabs captured by that request", () => {
    const closeAll = source.slice(
      source.indexOf("async function closeAllTabs"),
      source.indexOf("async function closeOtherTabs"),
    );
    expect(closeAll).toContain("const pathsAtRequest = tabState.tabs.map");
    expect(closeAll).toContain("await confirmDiscardDrafts(pathsAtRequest)");
    expect(closeAll).toContain("closeEditorTabs(tabState, pathsAtRequest)");
    expect(closeAll).not.toContain("closeAllEditorTabs()");

    const closeOthers = source.slice(
      source.indexOf("async function closeOtherTabs"),
      source.indexOf("function handleDraftChange"),
    );
    expect(closeOthers).toContain("const keepPath = tabState.active");
    expect(closeOthers).toContain("closeEditorTabs(tabState, pathsToClose)");
    expect(closeOthers).not.toContain("closeOtherEditorTabs(tabState");
  });

  it("clears a draft only after its write succeeds", () => {
    const save = source.slice(
      source.indexOf("async function handleFileSave"),
      source.indexOf("function openInDefaultApp"),
    );
    const write = save.indexOf('await invoke("cmd_write_file_content"');
    const complete = save.indexOf("completeCachedEditorSave");
    expect(write).toBeGreaterThan(-1);
    expect(complete).toBeGreaterThan(write);
    expect(save).not.toContain("catch (");
  });

  it("serializes the complete save lifecycle by canonical repository and path", () => {
    const save = source.slice(
      source.indexOf("async function handleFileSave"),
      source.indexOf("function openInDefaultApp"),
    );
    const queued = save.indexOf("fileSaveQueue.run(fileSaveKey(repo, path)");
    const write = save.indexOf('invoke("cmd_write_file_content"');
    const complete = save.indexOf("completeCachedEditorSave(repo, path, newContent)");
    const refresh = save.indexOf("repoStore.refresh()");
    expect(queued).toBeGreaterThan(-1);
    expect(write).toBeGreaterThan(queued);
    expect(complete).toBeGreaterThan(write);
    expect(refresh).toBeGreaterThan(complete);
  });

  it("completes saves from the latest cache without letting a stale instance mutate local state", () => {
    const complete = source.slice(
      source.indexOf("function completeCachedEditorSave"),
      source.indexOf("async function handleFileSave"),
    );
    const cacheRead = complete.indexOf("tabCache.get(repo)");
    const transition = complete.indexOf("completeEditorSave(cached.tabs, path, savedContent)");
    const cacheWrite = complete.indexOf("tabCache.set(repo");
    const ownerDispatch = complete.indexOf("viewerOwners.current(repo)?.completeSave");
    expect(cacheRead).toBeGreaterThan(-1);
    expect(transition).toBeGreaterThan(cacheRead);
    expect(cacheWrite).toBeGreaterThan(transition);
    expect(ownerDispatch).toBeGreaterThan(cacheWrite);
    expect(complete).toContain("recordEditorDrafts(repo, Object.keys(tabs.drafts))");
    expect(complete).not.toContain("completeEditorSave(tabState");
    expect(complete).not.toContain("tabState = tabs");
  });

  it("routes a settled save to the newest live viewer from latest cached tabs", () => {
    const apply = source.slice(
      source.indexOf("function applyCompletedSaveToCurrentViewer"),
      source.indexOf("function completeCachedEditorSave"),
    );
    expect(apply).toContain("if (!isCurrentViewer(repo)) return");
    expect(apply).toContain("tabState = tabs");
    expect(source).toContain("completeSave: (path, savedContent, tabs) =>");
    expect(source).toContain("applyCompletedSaveToCurrentViewer(repo, path, savedContent, tabs)");
  });

  it("binds writes to the repository that owns the displayed editor", () => {
    const save = source.slice(
      source.indexOf("async function handleFileSave"),
      source.indexOf("function openInDefaultApp"),
    );
    expect(save).toContain("const repo = hydratedRepo;");
    expect(save).toContain("$repoStore.currentPath !== repo");
    expect(save.indexOf("$repoStore.currentPath !== repo")).toBeLessThan(
      save.indexOf('await invoke("cmd_write_file_content"'),
    );
  });

  it("shows dirty state and forwards explicit draft/discard ownership", () => {
    expect(source).toContain("isEditorTabDirty(tabState, tab.path)");
    expect(source).toContain("Unsaved changes");
    expect(source).toContain("onRequestDiscard=");
  });

  it("does not apply a pending discard decision after the repository changes", () => {
    const guards = source.match(/!isCurrentViewer\(repo\)/g) ?? [];
    expect(guards.length).toBeGreaterThanOrEqual(4);
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

  it("arbitrates fixed side panes from the measured File view width", () => {
    expect(source).toContain("bind:this={fileViewRoot}");
    expect(source).toContain("new ResizeObserver");
    expect(source).toContain("resolveFilePaneLayout");
    expect(source).toContain("{#if paneLayout.explorerVisible}");
    expect(source).toContain("{#if paneLayout.editorVisible}");
    expect(source).toContain("{#if paneLayout.dashboardVisible}");
  });

  it("keeps the preferred side pane repository-scoped across view remounts", () => {
    expect(source).toContain("preferredSidePane: FileSidePane;");
    expect(source).toContain("preferredSidePane,\n    });");
    expect(source).toContain("preferredSidePane = cached.preferredSidePane ?? \"explorer\";");
    expect(source).toContain("preferredSidePane = \"explorer\";");
  });

  it("writes file content via cmd_write_file_content and opens paths through joinWorktreePath", () => {
    expect(source).toContain('invoke("cmd_write_file_content"');
    expect(source).toContain("joinWorktreePath");
    expect(source).toContain("formatError");
  });

  it("surfaces clipboard failure when copying the active path", () => {
    expect(source).toContain("if (!(await copyText(activeTabPath)))");
    expect(source).toContain('repoStore.setError("Could not copy path to clipboard")');
  });

  it("renders LanguageLogo for open editor tabs", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("filePath={tab.path}");
  });
});
