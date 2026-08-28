import { describe, expect, it } from "vitest";
import {
  activateEditorTab,
  closeAllEditorTabs,
  closeEditorTab,
  closeOtherEditorTabs,
  emptyEditorTabs,
  openPinned,
  openPreview,
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
});
