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

  it("lists the repository-tab digits as the host's command key, and the terminal claims no such chord", () => {
    const row = (os: "macos" | "windows" | "linux") =>
      terminalGuideShortcuts(os).find((entry) => entry.description.startsWith("Jump to a repository tab"));
    expect(row("macos")?.keys).toEqual(["⌘", "1–9"]);
    expect(row("windows")?.keys).toEqual(["Ctrl", "1–9"]);
    expect(row("linux")?.keys).toEqual(["Ctrl", "1–9"]);
    // The native accelerator is consumed before xterm, but the terminal's own
    // classifiers must not claim the chord either, or the two would race in a
    // browser build: Command+digit on macOS, Control+digit elsewhere.
    for (let digit = 1; digit <= 9; digit += 1) {
      const key = String(digit);
      expect(terminalTabChord({ ...chord, key, metaKey: true })).toBeNull();
      expect(terminalTabChord({ ...chord, key, ctrlKey: true })).toBeNull();
      expect(terminalViewChord({ ...chord, key, metaKey: true }, "macos")).toBeNull();
      expect(terminalViewChord({ ...chord, key, ctrlKey: true }, "windows")).toBeNull();
    }
  });

  it("lists the dock section chords for every host", () => {
    const row = (os: "macos" | "windows" | "linux") =>
      terminalGuideShortcuts(os).find((entry) => entry.description === "Dock: Shell/Agents/Files/Console");
    expect(row("macos")?.keys).toEqual(["⌘", "⌥", "⇧", "1–4"]);
    expect(row("windows")?.keys).toEqual(["Ctrl", "Alt", "Shift", "1–4"]);
    expect(row("linux")?.keys).toEqual(["Ctrl", "Alt", "Shift", "1–4"]);
  });

  it("keeps macOS line editing off other hosts", () => {
    expect(terminalGuideShortcuts("macos").some((row) => row.description === "Start of the line")).toBe(true);
    expect(terminalGuideShortcuts("linux").some((row) => row.description === "Start of the line")).toBe(false);
  });

  it("documents split creation, zoom, directional focus, cycling, slot jumps, and close-split on every host", () => {
    const mac = terminalGuideShortcuts("macos");
    expect(mac.find((r) => r.description === "Split terminal right")?.keys).toEqual(["⌘", "D"]);
    expect(mac.find((r) => r.description === "Split terminal down")?.keys).toEqual(["⌘", "⇧", "D"]);
    expect(mac.find((r) => r.description === "Zoom or restore split pane")?.keys).toEqual(["⌘", "⇧", "Enter"]);
    expect(mac.find((r) => r.description === "Focus adjacent split pane")?.keys).toEqual(["⌘", "⌥", "←/→/↑/↓"]);
    expect(mac.find((r) => r.description === "Previous / next visible split pane")?.keys).toEqual(["⌘", "⌥", "[ / ]"]);
    expect(mac.find((r) => r.description === "Previous / next terminal pair")?.keys).toEqual(["⌘", "⌥", "⇧", "[ / ]"]);
    expect(mac.find((r) => r.description === "Jump directly to split pane 1–8")?.keys).toEqual(["⌘", "⌥", "1–8"]);
    expect(mac.find((r) => r.description === "Close split view and keep active session")?.keys).toEqual(["⌘", "⌥", "0"]);

    const win = terminalGuideShortcuts("windows");
    expect(win.find((r) => r.description === "Split terminal right")?.keys).toEqual(["Ctrl", "Shift", "D"]);
    expect(win.find((r) => r.description === "Zoom or restore split pane")?.keys).toEqual(["Ctrl", "Shift", "Enter"]);
    expect(win.find((r) => r.description === "Previous / next visible split pane")?.keys).toEqual(["Ctrl", "Alt", "[ / ]"]);
    expect(win.find((r) => r.description === "Jump directly to split pane 1–8")?.keys).toEqual(["Ctrl", "Alt", "1–8"]);
    expect(win.find((r) => r.description === "Close split view and keep active session")?.keys).toEqual(["Ctrl", "Alt", "0"]);
  });
});
