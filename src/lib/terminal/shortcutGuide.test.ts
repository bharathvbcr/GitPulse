import { describe, expect, it } from "vitest";
import { terminalTabChord } from "./tabs";
import { terminalViewChord } from "./viewControls";
import { terminalGuideShortcuts } from "./shortcutGuide";

const chord = {
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
};

describe("terminal guide shortcuts", () => {
  it("prints the tab chords the dock binds on every host", () => {
    for (const os of ["macos", "windows", "linux"] as const) {
      const rows = terminalGuideShortcuts(os);
      expect(rows.find((row) => row.description.startsWith("New shell"))?.keys).toEqual(["Ctrl", "Shift", "T"]);
      expect(rows.find((row) => row.description.startsWith("Close session"))?.keys).toEqual(["Ctrl", "Shift", "W"]);
      expect(rows.find((row) => row.description === "Next terminal tab")?.keys).toEqual(["Ctrl", "Tab"]);
      expect(rows.find((row) => row.description === "Previous terminal tab")?.keys).toEqual(["Ctrl", "Shift", "Tab"]);
    }
    expect(terminalTabChord({ ...chord, key: "T", ctrlKey: true, shiftKey: true })).toBe("new");
    expect(terminalTabChord({ ...chord, key: "W", ctrlKey: true, shiftKey: true })).toBe("close");
    expect(terminalTabChord({ ...chord, key: "Tab", ctrlKey: true })).toBe("next");
    expect(terminalTabChord({ ...chord, key: "Tab", ctrlKey: true, shiftKey: true })).toBe("prev");
  });

  it("prints find and text size the way each host is actually bound", () => {
    expect(terminalGuideShortcuts("macos").find((row) => row.description === "Find in the terminal")?.keys).toEqual(["⌘", "F"]);
    expect(terminalGuideShortcuts("windows").find((row) => row.description === "Find in the terminal")?.keys).toEqual([
      "Ctrl",
      "Shift",
      "F",
    ]);
    expect(terminalViewChord({ ...chord, key: "f", metaKey: true }, "macos")).toBe("find");
    expect(terminalViewChord({ ...chord, key: "f", ctrlKey: true, shiftKey: true }, "windows")).toBe("find");
    expect(terminalViewChord({ ...chord, key: "f", ctrlKey: true }, "windows")).toBeNull();
  });

  it("keeps macOS line editing off other hosts", () => {
    expect(terminalGuideShortcuts("macos").some((row) => row.description === "Start of the line")).toBe(true);
    expect(terminalGuideShortcuts("linux").some((row) => row.description === "Start of the line")).toBe(false);
  });
});
