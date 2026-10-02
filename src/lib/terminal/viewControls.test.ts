import { describe, expect, it } from "vitest";
import { clampTerminalFontSize, macLineEditing, spawnGridSize, terminalSearchSummary, terminalViewChord } from "./viewControls";
import { initialState, openTab, terminalTabDestination } from "./tabs";

const key = (key: string, mods = {}) => ({ key, metaKey: false, ctrlKey: false, altKey: false, shiftKey: false, ...mods });

describe("terminal view controls", () => {
  it("keeps shell editing, IME and application chords intact", () => {
    for (const event of [key("f", { ctrlKey: true }), key("k", { metaKey: true }), key("c", { ctrlKey: true }), key("f", { metaKey: true, altKey: true }), key("f", { metaKey: true, isComposing: true }), key("f", { metaKey: true, keyCode: 229 }), key("f", { metaKey: true, ctrlKey: true }), key("f")]) {
      expect(terminalViewChord(event)).toBeNull();
    }
  });

  it("finds and zooms with Command or Control+Shift", () => {
    for (const mods of [{ metaKey: true }, { ctrlKey: true, shiftKey: true }]) {
      expect(terminalViewChord(key("F", mods))).toBe("find");
      for (const plus of ["=", "+"]) expect(terminalViewChord(key(plus, mods))).toBe("zoom-in");
      for (const minus of ["-", "_"]) expect(terminalViewChord(key(minus, mods))).toBe("zoom-out");
      for (const zero of ["0", ")"]) expect(terminalViewChord(key(zero, mods))).toBe("zoom-reset");
    }
  });

  it("leaves Control+Shift to the shell on macOS, where Ctrl+Shift+- is readline's undo", () => {
    for (const k of ["F", "=", "+", "-", "_", "0", ")"]) {
      expect(terminalViewChord(key(k, { ctrlKey: true, shiftKey: true }), "macos")).toBeNull();
    }
    expect(terminalViewChord(key("F", { metaKey: true }), "macos")).toBe("find");
    expect(terminalViewChord(key("-", { metaKey: true }), "macos")).toBe("zoom-out");
    for (const os of ["windows", "linux", "unknown"]) {
      expect(terminalViewChord(key("_", { ctrlKey: true, shiftKey: true }), os)).toBe("zoom-out");
    }
  });

  it("maps the Mac line-editing keys at a shell prompt", () => {
    const at = (k: string, mods = {}) => macLineEditing(key(k, mods), "macos", false);
    expect(at("ArrowLeft", { metaKey: true })).toBe("\x01");
    expect(at("ArrowRight", { metaKey: true })).toBe("\x05");
    expect(at("Backspace", { metaKey: true })).toBe("\x15");
    expect(at("ArrowLeft", { altKey: true })).toBe("\x1bb");
    expect(at("ArrowRight", { altKey: true })).toBe("\x1bf");
  });

  it("leaves every other key, platform, screen and composition alone", () => {
    const left = key("ArrowLeft", { metaKey: true });
    expect(macLineEditing(left, "windows", false)).toBeNull();
    expect(macLineEditing(left, "linux", false)).toBeNull();
    expect(macLineEditing(left, undefined, false)).toBeNull();
    expect(macLineEditing(left, "macos", true)).toBeNull();
    expect(macLineEditing({ ...left, isComposing: true }, "macos", false)).toBeNull();
    expect(macLineEditing({ ...left, keyCode: 229 }, "macos", false)).toBeNull();
    for (const mods of [{ metaKey: true, shiftKey: true }, { metaKey: true, ctrlKey: true }, { metaKey: true, altKey: true }, {}]) {
      expect(macLineEditing(key("ArrowLeft", mods), "macos", false)).toBeNull();
    }
    for (const k of ["ArrowUp", "ArrowDown", "a", "Delete", "Enter"]) {
      expect(macLineEditing(key(k, { metaKey: true }), "macos", false)).toBeNull();
    }
    expect(macLineEditing(key("Backspace", { altKey: true }), "macos", false)).toBeNull();
  });

  it("opens a PTY at a whole, finite size even when a hidden tab measures NaN", () => {
    expect(spawnGridSize(undefined)).toEqual({ rows: 24, cols: 80 });
    expect(spawnGridSize(null)).toEqual({ rows: 24, cols: 80 });
    expect(spawnGridSize({ rows: NaN, cols: NaN })).toEqual({ rows: 24, cols: 80 });
    expect(spawnGridSize({ rows: Infinity, cols: -Infinity })).toEqual({ rows: 24, cols: 80 });
    expect(spawnGridSize({ rows: 0, cols: 1 })).toEqual({ rows: 2, cols: 2 });
    expect(spawnGridSize({ rows: 40.7, cols: 120.2 })).toEqual({ rows: 40, cols: 120 });
    expect(spawnGridSize({ rows: 1e9, cols: 5000 })).toEqual({ rows: 1000, cols: 1000 });
    expect(JSON.stringify(spawnGridSize({ rows: NaN, cols: 80 }))).toBe('{"rows":24,"cols":80}');
  });

  it("bounds text size, including malformed and fractional requests", () => {
    expect([NaN, Infinity, -Infinity].map(clampTerminalFontSize)).toEqual([12, 12, 12]);
    expect([-10, 10, 12.6, 24, 1000].map(clampTerminalFontSize)).toEqual([10, 10, 13, 24, 24]);
  });

  it("distinguishes no matches, selection, and capped results", () => {
    expect(terminalSearchSummary(-1, 0)).toBe("No matches");
    expect(terminalSearchSummary(2, 9)).toBe("3 of 9");
    expect(terminalSearchSummary(-1, 9)).toBe("9 matches");
    expect(terminalSearchSummary(-1, 1000)).toBe("1000+ matches");
  });

  it("navigates the strip at both edges without sending arrows to a shell", () => {
    const state = openTab(initialState(), "shell");
    expect(terminalTabDestination(state, "Home")).toBe(state.tabs[0].id);
    expect(terminalTabDestination(state, "End")).toBe(state.tabs[1].id);
    expect(terminalTabDestination(state, "ArrowRight")).toBe(state.tabs[0].id);
    expect(terminalTabDestination(state, "ArrowLeft")).toBe(state.tabs[0].id);
    expect(terminalTabDestination(state, "Enter")).toBeNull();
    expect(terminalTabDestination({ tabs: [], activeId: null }, "Home")).toBeNull();
  });
});
