import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ShortcutsModal from "./ShortcutsModal.svelte";

describe("ShortcutsModal", () => {
  it("renders nothing when closed", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: false } });
    expect(body).not.toContain('role="dialog"');
  });

  it("renders modal dialog with shortcut categories and keycaps when open", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Keyboard Shortcuts"');
    expect(body).toContain("Workspace");
    expect(body).toContain("Navigation");
    expect(body).toContain("Open Command Palette");
  });

  it("documents the same Open, Clone, and Work accelerators the native menu binds", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Open Repository…");
    expect(body).toContain("Clone Repository…");
    expect(body).toContain("Open Work");
    expect(body).toContain("F10");
    // Native File menu binds CmdOrCtrl+O and CmdOrCtrl+Shift+O; the sheet
    // used to list only ⌘T, so asking for help hid the chords the menu uses.
    expect(body).toContain("O");
  });

  it("documents that commit search switches to Graph rather than no-opping on Work", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Search commits");
    expect(body).toContain("switches to Graph");
  });

  it("names the views ⌘-digit actually reaches", () => {
    // The sheet listed nine retired views — "Files, Graph, Diff, Resolve,
    // Blame, Stack, GitHub, Coverage, Health" — long after the consolidation
    // left three digits bound to three views.
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Code, History, Insights");
    expect(body).not.toContain("Resolve, Blame, Stack");
  });

  it("documents the diff's own chords, which have no menu entry to find them by", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Find in this diff");
    expect(body).toContain("Previous / next block of changes");
    expect(body).toContain("Previous / next file in this change");
    expect(body).toContain("F3");
  });
});
