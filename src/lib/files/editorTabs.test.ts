import { describe, expect, it } from "vitest";
import {
  activateEditorTab,
  closeAllEditorTabs,
  closeEditorTabs,
  closeEditorTab,
  closeOtherEditorTabs,
  dirtyEditorTabPaths,
  emptyEditorTabs,
  openPinned,
  openPreview,
  pinEditorTab,
} from "./editorTabs";

// Drafts are the open-file store's (`openDocuments.test.ts`); the tabs are
// told which paths are dirty.
const dirtyAmong = (...paths: string[]) => (path: string) => paths.includes(path);

describe("editorTabs", () => {
  it("reuses a single preview tab on browse", () => {
    let state = emptyEditorTabs();
    state = openPreview(state, "src/a.ts");
    expect(state.tabs).toEqual([{ path: "src/a.ts", name: "a.ts", preview: true }]);
    state = openPreview(state, "src/b.ts");
    expect(state.tabs).toHaveLength(1);
    expect(state.tabs[0]).toEqual({ path: "src/b.ts", name: "b.ts", preview: true });
    expect(state.active).toBe("src/b.ts");
  });

  it("does not replace a pinned tab when previewing another file", () => {
    let state = openPinned(emptyEditorTabs(), "src/a.ts");
    state = openPreview(state, "src/b.ts");
    expect(state.tabs.map((t) => t.path)).toEqual(["src/a.ts", "src/b.ts"]);
    expect(state.tabs[0].preview).toBe(false);
    expect(state.tabs[1].preview).toBe(true);
  });

  it("promotes a preview tab to pinned without duplicating it", () => {
    let state = openPreview(emptyEditorTabs(), "src/a.ts");
    state = openPinned(state, "src/a.ts");
    expect(state.tabs).toEqual([{ path: "src/a.ts", name: "a.ts", preview: false }]);
    state = openPinned(state, "src/a.ts");
    expect(state.tabs).toHaveLength(1);
  });

  it("closes the active tab onto a neighbor and close-others keeps one pinned", () => {
    let state = openPinned(emptyEditorTabs(), "a.ts");
    state = openPinned(state, "b.ts");
    state = openPinned(state, "c.ts");
    state = closeEditorTab(state, "b.ts");
    expect(state.tabs.map((t) => t.path)).toEqual(["a.ts", "c.ts"]);
    expect(state.active).toBe("c.ts");
    state = closeOtherEditorTabs(state, "a.ts");
    expect(state.tabs).toEqual([{ path: "a.ts", name: "a.ts", preview: false }]);
    expect(closeAllEditorTabs().tabs).toEqual([]);
  });

  it("activates an open tab without pinning it", () => {
    let state = openPreview(emptyEditorTabs(), "src/a.ts");
    state = openPinned(state, "src/b.ts");
    state = activateEditorTab(state, "src/a.ts");
    expect(state.active).toBe("src/a.ts");
    expect(state.tabs.find((t) => t.path === "src/a.ts")?.preview).toBe(true);
    expect(activateEditorTab(state, "missing.ts")).toBe(state);
  });

  it("ignores empty paths", () => {
    const empty = emptyEditorTabs();
    expect(openPreview(empty, "")).toBe(empty);
    expect(openPinned(empty, "")).toBe(empty);
  });

  it("pins dirty previews so rapid browsing cannot replace a tab that owns unsaved work", () => {
    const dirty = dirtyAmong("src/a.ts", "src/b.ts");
    let state = openPreview(emptyEditorTabs(), "src/a.ts");
    state = openPreview(state, "src/b.ts", dirty);
    state = openPreview(state, "src/c.ts", dirty);

    expect(state.tabs.map(({ path, preview }) => ({ path, preview }))).toEqual([
      { path: "src/a.ts", preview: false },
      { path: "src/b.ts", preview: false },
      { path: "src/c.ts", preview: true },
    ]);
  });

  it("identifies only dirty tabs a close operation would actually remove", () => {
    let state = openPinned(emptyEditorTabs(), "a.ts");
    state = openPinned(state, "b.ts");
    state = openPinned(state, "clean.ts");
    const dirty = dirtyAmong("a.ts", "b.ts");

    expect(dirtyEditorTabPaths(state, ["clean.ts"], dirty)).toEqual([]);
    expect(dirtyEditorTabPaths(state, ["b.ts", "clean.ts", "a.ts"], dirty)).toEqual(["a.ts", "b.ts"]);
    // A path with no tab is not a tab to close, dirty or not.
    expect(dirtyEditorTabPaths(state, ["gone.ts"], () => true)).toEqual([]);
    expect(closeOtherEditorTabs(state, "a.ts").tabs.map((tab) => tab.path)).toEqual(["a.ts"]);
    expect(closeEditorTab(state, "a.ts").tabs.map((tab) => tab.path)).toEqual(["b.ts", "clean.ts"]);
    expect(closeAllEditorTabs()).toEqual({ tabs: [], active: null });
  });

  it("closes only the paths captured by an asynchronous tab operation", () => {
    let state = openPinned(emptyEditorTabs(), "a.ts");
    state = openPinned(state, "b.ts");
    const pathsAtRequest = state.tabs.map((tab) => tab.path);

    // A tab opened while a save or confirmation was pending was not part of
    // the user's original request and must survive its eventual completion.
    state = openPinned(state, "opened-during-wait.ts");
    const closed = closeEditorTabs(state, pathsAtRequest);

    expect(closed.tabs.map((tab) => tab.path)).toEqual(["opened-during-wait.ts"]);
    expect(closed.active).toBe("opened-during-wait.ts");
  });

  it("pins the close-others target without stealing a newer selection", () => {
    let state = openPreview(emptyEditorTabs(), "keep.ts");
    state = openPinned(state, "close.ts");
    state = activateEditorTab(state, "keep.ts");
    const pathsToClose = ["close.ts"];

    state = openPinned(state, "opened-during-wait.ts");
    const closed = pinEditorTab(closeEditorTabs(state, pathsToClose), "keep.ts");

    expect(closed.tabs.map(({ path, preview }) => ({ path, preview }))).toEqual([
      { path: "keep.ts", preview: false },
      { path: "opened-during-wait.ts", preview: false },
    ]);
    expect(closed.active).toBe("opened-during-wait.ts");
  });
});
