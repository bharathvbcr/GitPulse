import { describe, expect, it } from "vitest";
import {
  activateEditorTab,
  completeEditorSave,
  closeAllEditorTabs,
  closeEditorTabs,
  closeEditorTab,
  closeOtherEditorTabs,
  dirtyEditorTabPaths,
  editorDraft,
  emptyEditorTabs,
  isEditorDraftConflicted,
  isEditorTabDirty,
  openPinned,
  openPreview,
  pinEditorTab,
  updateEditorDraft,
} from "./editorTabs";

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

  it("pins dirty previews and preserves per-path drafts during rapid switching", () => {
    let state = openPreview(emptyEditorTabs(), "src/a.ts");
    state = updateEditorDraft(state, "src/a.ts", "draft a", "disk a");
    state = openPreview(state, "src/b.ts");
    state = updateEditorDraft(state, "src/b.ts", "draft b", "disk b");
    state = openPreview(state, "src/c.ts");

    expect(state.tabs.map(({ path, preview }) => ({ path, preview }))).toEqual([
      { path: "src/a.ts", preview: false },
      { path: "src/b.ts", preview: false },
      { path: "src/c.ts", preview: true },
    ]);
    expect(editorDraft(state, "src/a.ts")?.content).toBe("draft a");
    expect(editorDraft(state, "src/b.ts")?.content).toBe("draft b");
    expect(isEditorTabDirty(state, "src/a.ts")).toBe(true);
  });

  it("clears dirty state only through successful-save completion", () => {
    let state = openPinned(emptyEditorTabs(), "src/a.ts");
    state = updateEditorDraft(state, "src/a.ts", "new value", "old value");

    // A rejected save returns the existing state without calling the success
    // transition, so the draft remains the canonical editor value.
    expect(editorDraft(state, "src/a.ts")?.content).toBe("new value");
    const saved = completeEditorSave(state, "src/a.ts", "new value");
    expect(isEditorTabDirty(saved, "src/a.ts")).toBe(false);
    expect(editorDraft(saved, "src/a.ts")).toBeUndefined();

    const newer = updateEditorDraft(state, "src/a.ts", "newer value", "old value");
    expect(completeEditorSave(newer, "src/a.ts", "new value")).toBe(newer);
    expect(editorDraft(newer, "src/a.ts")?.content).toBe("newer value");
  });

  it("identifies only dirty drafts a close operation would actually remove", () => {
    let state = openPinned(emptyEditorTabs(), "a.ts");
    state = openPinned(state, "b.ts");
    state = openPinned(state, "clean.ts");
    state = updateEditorDraft(state, "a.ts", "draft a", "disk a");
    state = updateEditorDraft(state, "b.ts", "draft b", "disk b");

    expect(dirtyEditorTabPaths(state, ["clean.ts"])).toEqual([]);
    expect(dirtyEditorTabPaths(state, ["b.ts", "clean.ts", "a.ts"])).toEqual([
      "a.ts",
      "b.ts",
    ]);
    const keepA = closeOtherEditorTabs(state, "a.ts");
    expect(editorDraft(keepA, "a.ts")?.content).toBe("draft a");
    expect(editorDraft(keepA, "b.ts")).toBeUndefined();
    expect(editorDraft(closeEditorTab(state, "a.ts"), "a.ts")).toBeUndefined();
    expect(closeAllEditorTabs().drafts).toEqual({});
  });

  it("closes only the paths captured by an asynchronous tab operation", () => {
    let state = openPinned(emptyEditorTabs(), "a.ts");
    state = openPinned(state, "b.ts");
    const pathsAtRequest = state.tabs.map((tab) => tab.path);

    // A tab opened while a save or confirmation was pending was not part of
    // the user's original request and must survive its eventual completion.
    state = openPinned(state, "opened-during-wait.ts");
    state = updateEditorDraft(state, "opened-during-wait.ts", "draft", "disk");
    const closed = closeEditorTabs(state, pathsAtRequest);

    expect(closed.tabs.map((tab) => tab.path)).toEqual(["opened-during-wait.ts"]);
    expect(closed.active).toBe("opened-during-wait.ts");
    expect(editorDraft(closed, "opened-during-wait.ts")?.content).toBe("draft");
  });

  it("pins the close-others target without stealing a newer selection", () => {
    let state = openPreview(emptyEditorTabs(), "keep.ts");
    state = openPinned(state, "close.ts");
    state = activateEditorTab(state, "keep.ts");
    const pathsToClose = ["close.ts"];

    state = openPinned(state, "opened-during-wait.ts");
    state = updateEditorDraft(state, "opened-during-wait.ts", "draft", "disk");
    const closed = pinEditorTab(closeEditorTabs(state, pathsToClose), "keep.ts");

    expect(closed.tabs.map(({ path, preview }) => ({ path, preview }))).toEqual([
      { path: "keep.ts", preview: false },
      { path: "opened-during-wait.ts", preview: false },
    ]);
    expect(closed.active).toBe("opened-during-wait.ts");
    expect(editorDraft(closed, "opened-during-wait.ts")?.content).toBe("draft");
  });

  it("drops a draft when edits return exactly to their loaded content", () => {
    let state = openPreview(emptyEditorTabs(), "a.ts");
    state = updateEditorDraft(state, "a.ts", "changed", "original");
    state = updateEditorDraft(state, "a.ts", "original", "original");
    expect(isEditorTabDirty(state, "a.ts")).toBe(false);
  });

  describe("the disk version a draft was made from", () => {
    const dirty = () => {
      let state = openPinned(emptyEditorTabs(), "a.ts");
      state = updateEditorDraft(state, "a.ts", "mine", "v1 text", "3:v1");
      return state;
    };

    it("is recorded by the first dirtying keystroke", () => {
      expect(editorDraft(dirty(), "a.ts")).toEqual({
        content: "mine",
        sourceContent: "v1 text",
        base: "3:v1",
      });
    });

    it("is kept when later keystrokes arrive carrying a reloaded blob", () => {
      // The file changed on disk and its blob reloaded: the editor now reports
      // the new text and base with every keystroke. The draft must not adopt
      // them, or the external change would stop being a conflict.
      const state = updateEditorDraft(dirty(), "a.ts", "mine!", "agent text", "9:v2");
      expect(editorDraft(state, "a.ts")).toEqual({
        content: "mine!",
        sourceContent: "v1 text",
        base: "3:v1",
      });
    });

    it("drops the draft when it is edited back to the text it was made from", () => {
      const state = updateEditorDraft(dirty(), "a.ts", "v1 text", "agent text", "9:v2");
      expect(isEditorTabDirty(state, "a.ts")).toBe(false);
    });

    it("re-bases a draft that is still dirty when an older snapshot is saved", () => {
      const newer = updateEditorDraft(dirty(), "a.ts", "mine plus more", "v1 text", "3:v1");
      const after = completeEditorSave(newer, "a.ts", "mine", "4:saved");
      expect(editorDraft(after, "a.ts")).toEqual({
        content: "mine plus more",
        sourceContent: "mine",
        base: "4:saved",
      });
    });

    it("clears the draft when the saved text is the draft", () => {
      expect(isEditorTabDirty(completeEditorSave(dirty(), "a.ts", "mine", "4:saved"), "a.ts")).toBe(
        false,
      );
    });

    it("conflicts when the reloaded blob carries a different base than the draft", () => {
      const draft = editorDraft(dirty(), "a.ts");
      expect(isEditorDraftConflicted(draft, "9:v2")).toBe(true);
      expect(isEditorDraftConflicted(draft, "3:v1")).toBe(false);
      expect(isEditorDraftConflicted(draft, null)).toBe(false);
      expect(isEditorDraftConflicted(draft, undefined)).toBe(false);
      expect(isEditorDraftConflicted(undefined, "9:v2")).toBe(false);
      const unbased = editorDraft(
        updateEditorDraft(openPinned(emptyEditorTabs(), "a.ts"), "a.ts", "x", "y"),
        "a.ts",
      );
      expect(isEditorDraftConflicted(unbased, "9:v2")).toBe(false);
    });
  });
});
