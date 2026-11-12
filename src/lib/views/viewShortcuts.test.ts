import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { REGISTERED_VIEWS, sectionsFor } from "./viewRegistry";
import { classifyShortcut } from "../ui/webviewShortcuts";
import {
  repoTabChordModifiers,
  SECTION_KEY_CODES,
  sectionForChord,
  sectionShortcutRows,
  VIEW_DIGIT_ORDER,
  viewAccelerator,
  viewChordModifiers,
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
  /** One table of `menu.rs`, as (view, modifiers, digit) rows. */
  function menuTable(name: string) {
    const start = menuRs.indexOf(`const ${name}:`);
    const table = menuRs.slice(start, menuRs.indexOf("];", start));
    return [...table.matchAll(/actions::(\w+),\s*"[^"]+",\s*"([^"]+)"/g)].map(([, id, accel]) => {
      const tokens = accel.split("+");
      const digit = Number(tokens.pop());
      return { id, digit, modifiers: tokens };
    });
  }

  /** menu.rs spells CmdOrCtrl; resolve it for the host the table is for. */
  const resolveModifier = (token: string, os: "macos" | "windows") => {
    switch (token.toLowerCase()) {
      case "cmdorctrl":
        return os === "macos" ? "meta" : "ctrl";
      case "cmd":
        return "meta";
      case "ctrl":
        return "ctrl";
      case "alt":
        return "alt";
      case "shift":
        return "shift";
      default:
        throw new Error(`unknown modifier ${token}`);
    }
  };

  it("binds the same views, in the same order, as the native view tables", () => {
    for (const [table, os] of [
      ["VIEW_TAB_BINDINGS_MACOS", "macos"],
      ["VIEW_TAB_BINDINGS_OTHER", "windows"],
    ] as const) {
      const bound = menuTable(table);
      expect(bound.map((b) => b.id.replace("TAB_", "").toLowerCase()), table).toEqual([
        ...VIEW_DIGIT_ORDER,
      ]);
      expect(bound.map((b) => b.digit), table).toEqual(bound.map((_, i) => i + 1));
      // The chord's modifiers are the structured table's, not a second copy.
      for (const row of bound) {
        expect(
          row.modifiers.map((token) => resolveModifier(token, os)).sort(),
          `${table} ${row.id}`,
        ).toEqual([...viewChordModifiers(os)].sort());
      }
    }
  });

  it("binds the repository-tab digits to the command key alone, the same on every host", () => {
    const bound = menuTable("REPO_TAB_BINDINGS");
    expect(bound.map((b) => b.digit)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9]);
    for (const os of ["macos", "windows"] as const) {
      for (const row of bound) {
        expect(row.modifiers.map((token) => resolveModifier(token, os))).toEqual([
          ...repoTabChordModifiers(os),
        ]);
      }
    }
  });

  it("gives Work the accelerator the menu actually assigns it", () => {
    expect(menuRs).toContain('actions::TAB_WORK, "Work", Some("F10")');
    expect(viewAccelerator("work")).toBe("F10");
  });

  it("prints the chord each host actually binds, never a mechanical translation", () => {
    expect(viewAccelerator("code", "macos")).toBe("⌥⌘1");
    expect(viewAccelerator("history", "macos")).toBe("⌥⌘2");
    expect(viewAccelerator("insights", "macos")).toBe("⌥⌘3");
    for (const os of ["windows", "linux"] as const) {
      expect(viewAccelerator("code", os)).toBe("Ctrl+Shift+1");
      expect(viewAccelerator("insights", os)).toBe("Ctrl+Shift+3");
    }
    // The cheat-sheet chips follow the same table: modifiers, then the digit.
    expect(viewShortcutRows("macos").find((row) => row.description === "Open Code")?.keys).toEqual([
      "⌥",
      "⌘",
      "1",
    ]);
    expect(viewShortcutRows("windows").find((row) => row.description === "Open Code")?.keys).toEqual([
      "⌃",
      "⇧",
      "1",
    ]);
  });

  it("keeps every view chord off the screenshot shortcuts and off AltGr", () => {
    const view = (os: "macos" | "windows" | "linux") => new Set(viewChordModifiers(os));
    // macOS ⌘⇧3/4/5 are the system screenshot shortcuts.
    expect(view("macos")).not.toEqual(new Set(["meta", "shift"]));
    // Ctrl+Alt is AltGr on many Windows and Linux layouts.
    for (const os of ["windows", "linux"] as const) {
      expect(view(os).has("ctrl") && view(os).has("alt")).toBe(false);
    }
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
      sheet.indexOf("function shortcutCategories"),
      sheet.indexOf("</script>"),
    );
    const descriptions = [...table.matchAll(/description: "([^"]+)"/g)].map(([, d]) => d);
    for (const description of descriptions) {
      expect(description).not.toContain("Files, Graph, Diff, Resolve");
      expect(description).not.toMatch(/Jump to view:/);
    }
  });

  it("renders the derived rows rather than a hand-written list", () => {
    expect(sheet).toContain("...viewShortcutRows(os)");
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
    // The chord has to be exactly one past the end or this passes vacuously,
    // so it is derived from the section count rather than written out: History
    // gained a fourth lens and a hard-coded "Digit5" would have gone on
    // testing the same thing while meaning something else.
    const sections = sectionsFor("history");
    expect(sections.length).toBeLessThan(SECTION_KEY_CODES.length);
    expect(sectionForChord("history", chord(SECTION_KEY_CODES[sections.length]))).toBeNull();
    // And the last real section still resolves, so "past the end" is a
    // boundary rather than the whole map having stopped matching.
    expect(sectionForChord("history", chord(SECTION_KEY_CODES[sections.length - 1]))?.id).toBe(
      sections[sections.length - 1].id,
    );
  });

  it("ignores every digit chord another feature owns", () => {
    // One keystroke with two owners is the defect that put Fleet on the
    // context-menu key; it must not be reintroduced here. ⌘/Ctrl+digit select
    // repository tabs (with Alt added they would still be someone else's), and
    // ⌥⌘digit and Ctrl+Shift+digit select views.
    expect(sectionForChord("history", chord("Digit1", { ctrl: true }))).toBeNull();
    expect(sectionForChord("history", chord("Digit1", { meta: true }))).toBeNull();
    expect(sectionForChord("history", chord("Digit1", { ctrl: true, shift: true, alt: false }))).toBeNull();
  });

  it("is disjoint from the repository-tab chord and from the view chord on every host", () => {
    const hosts = ["macos", "windows", "linux"] as const;
    for (const os of hosts) {
      const asChord = (modifiers: readonly string[], code: string) => ({
        alt: modifiers.includes("alt"),
        ctrl: modifiers.includes("ctrl"),
        meta: modifiers.includes("meta"),
        shift: modifiers.includes("shift"),
        code,
      });
      const repo = repoTabChordModifiers(os);
      const views = viewChordModifiers(os);
      // Different modifier sets, so no keystroke can be both.
      expect([...repo].sort()).not.toEqual([...views].sort());
      for (const code of SECTION_KEY_CODES) {
        // Neither is a section chord (⌥ alone)…
        expect(sectionForChord("history", asChord(repo, code)), `${os} repo ${code}`).toBeNull();
        expect(sectionForChord("history", asChord(views, code)), `${os} view ${code}`).toBeNull();
        // …and neither is a repository-tab jump when typed as the other.
        const event = (modifiers: readonly string[]) => ({
          key: code.replace("Digit", ""),
          code,
          ctrlKey: modifiers.includes("ctrl"),
          altKey: modifiers.includes("alt"),
          metaKey: modifiers.includes("meta"),
          shiftKey: modifiers.includes("shift"),
        });
        expect(classifyShortcut(event(repo), os === "macos")).toBe("jumpToTab");
        expect(classifyShortcut(event(views), os === "macos"), `${os} view ${code}`).not.toBe("jumpToTab");
      }
    }
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
