import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { REGISTERED_VIEWS, sectionsFor } from "./viewRegistry";
import {
  SECTION_KEY_CODES,
  sectionForChord,
  sectionShortcutRows,
  VIEW_DIGIT_ORDER,
  viewAccelerator,
  viewShortcutRows,
} from "./viewShortcuts";

const menuRs = readFileSync(
  new URL("../../../src-tauri/src/desktop/menu.rs", import.meta.url),
  "utf8",
);
const sheet = readFileSync(
  new URL("../components/ShortcutsModal.svelte", import.meta.url),
  "utf8",
);

const chord = (code: string, over: Partial<Parameters<typeof sectionForChord>[1]> = {}) => ({
  alt: true,
  ctrl: false,
  meta: false,
  shift: false,
  code,
  ...over,
});

describe("the digit table matches the native menu", () => {
  /**
   * The cheat sheet and the menu are two statements of one fact. They drifted
   * once already — the sheet still listed nine retired views against a menu
   * that bound three — so the table is asserted against the Rust source rather
   * than trusted.
   */
  it("binds the same views, in the same order, as VIEW_TAB_BINDINGS", () => {
    const table = menuRs.slice(
      menuRs.indexOf("const VIEW_TAB_BINDINGS"),
      menuRs.indexOf("];", menuRs.indexOf("const VIEW_TAB_BINDINGS")),
    );
    const bound = [...table.matchAll(/actions::TAB_(\w+),\s*"[^"]+",\s*"CmdOrCtrl\+(\d)"/g)].map(
      ([, name, digit]) => ({ view: name.toLowerCase(), digit: Number(digit) }),
    );
    expect(bound.map((b) => b.view)).toEqual([...VIEW_DIGIT_ORDER]);
    expect(bound.map((b) => b.digit)).toEqual(bound.map((_, i) => i + 1));
  });

  it("gives Work the accelerator the menu actually assigns it", () => {
    expect(menuRs).toContain('actions::TAB_WORK, "Work", Some("F10")');
    expect(viewAccelerator("work")).toBe("F10");
  });

  it("has an accelerator for every registered view", () => {
    for (const view of REGISTERED_VIEWS) {
      expect(viewAccelerator(view.id), `${view.id} has no chord`).not.toBeNull();
    }
  });
});

describe("the cheat sheet is derived, not restated", () => {
  it("no longer names views that were retired by the consolidation", () => {
    // Scoped to the shortcut DATA: the surrounding comment quotes the old
    // string on purpose, to say what went wrong.
    const table = sheet.slice(
      sheet.indexOf("const SHORTCUT_CATEGORIES"),
      sheet.indexOf("</script>"),
    );
    const descriptions = [...table.matchAll(/description: "([^"]+)"/g)].map(([, d]) => d);
    for (const description of descriptions) {
      expect(description).not.toContain("Files, Graph, Diff, Resolve");
      expect(description).not.toMatch(/Jump to view:/);
    }
  });

  it("renders the derived rows rather than a hand-written list", () => {
    expect(sheet).toContain("...viewShortcutRows()");
    expect(sheet).toContain("...sectionShortcutRows()");
  });

  it("produces one row per view and one per sectioned view", () => {
    expect(viewShortcutRows()).toHaveLength(REGISTERED_VIEWS.length);
    const sectioned = REGISTERED_VIEWS.filter((v) => sectionsFor(v.id).length > 1);
    expect(sectionShortcutRows()).toHaveLength(sectioned.length);
  });

  it("names every section it claims a chord for", () => {
    for (const row of sectionShortcutRows()) {
      const view = REGISTERED_VIEWS.find((v) => row.description.startsWith(v.label));
      expect(view).toBeDefined();
      for (const section of sectionsFor(view!.id)) {
        expect(row.description).toContain(section.label);
      }
    }
  });
});

describe("sectionForChord", () => {
  it("selects the nth section of the active view", () => {
    const sections = sectionsFor("history");
    expect(sectionForChord("history", chord("Digit2"))?.id).toBe(sections[1].id);
  });

  it("matches on code, not key, so ⌥1 works on a Mac layout", () => {
    // ⌥1 emits "¡"; a `key`-based test would never fire.
    expect(SECTION_KEY_CODES[0]).toBe("Digit1");
    expect(sectionForChord("history", chord("Digit1"))).not.toBeNull();
  });

  it("returns null past the end rather than clamping to the last section", () => {
    // ⌥5 in a three-section view must fall through, not land on Reflog.
    expect(sectionsFor("history")).toHaveLength(3);
    expect(sectionForChord("history", chord("Digit5"))).toBeNull();
  });

  it("ignores Ctrl+Alt+digit, which already switches repository tabs", () => {
    // One keystroke with two owners is the defect that put Fleet on the
    // context-menu key; it must not be reintroduced here.
    expect(sectionForChord("history", chord("Digit1", { ctrl: true }))).toBeNull();
  });

  it("ignores the chord without Alt, and with Meta or Shift added", () => {
    expect(sectionForChord("history", chord("Digit1", { alt: false }))).toBeNull();
    expect(sectionForChord("history", chord("Digit1", { meta: true }))).toBeNull();
    expect(sectionForChord("history", chord("Digit1", { shift: true }))).toBeNull();
  });

  it("returns null for a view with no sections", () => {
    for (const view of REGISTERED_VIEWS) {
      if (sectionsFor(view.id).length > 0) continue;
      expect(sectionForChord(view.id, chord("Digit1"))).toBeNull();
    }
  });
});

describe("Fleet is off the platform context-menu key", () => {
  it("does not bind Shift+F10 in the native menu", () => {
    // CommitRow and FileTreePanel implement Shift+F10 as "open the context
    // menu"; a native accelerator is consumed before the webview sees it.
    expect(menuRs).not.toContain('Some("Shift+F10")');
    expect(menuRs).toContain('const FLEET_ACCEL: &str = "CmdOrCtrl+Shift+F"');
  });

  it("says so in the cheat sheet", () => {
    expect(sheet).toContain("Open the Fleet dashboard");
  });
});
