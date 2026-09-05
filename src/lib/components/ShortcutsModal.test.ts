import { describe, expect, it } from "vitest";
import { COMMIT_FILTER_VIEW } from "../views/commitFilter";
import { VIEW_REGISTRY } from "../views/viewRegistry";
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

  it("documents where commit search actually lands, not where it used to", () => {
    // The sheet said "switches to Graph" long after Graph stopped being a
    // view; COMMIT_FILTER_VIEW is the fact, so assert against that rather
    // than against a second hand-written copy of it.
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Search commits");
    const label = VIEW_REGISTRY[COMMIT_FILTER_VIEW].label;
    expect(body).toContain(`switches to ${label}`);
  });

  it("names the views ⌘-digit actually reaches", () => {
    // The sheet listed nine retired views — "Files, Graph, Diff, Resolve,
    // Blame, Stack, GitHub, Coverage, Health" — long after the consolidation
    // left three digits bound to three views.
    // The list is derived from the registry now rather than written out, so
    // each view is named beside the digit that actually reaches it — a prose
    // list could go stale again; a per-row mapping cannot.
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Open Code");
    expect(body).toContain("Open History");
    expect(body).toContain("Open Insights");
    expect(body).not.toContain("Resolve, Blame, Stack");
    expect(body).not.toContain("⌘1–9");
  });

  it("documents the diff's own chords, which have no menu entry to find them by", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain("Find in this diff");
    expect(body).toContain("Previous / next block of changes");
    expect(body).toContain("Previous / next file in this change");
    expect(body).toContain("F3");
  });
});
