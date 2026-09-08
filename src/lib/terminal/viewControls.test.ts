import { describe, expect, it } from "vitest";
import { clampTerminalFontSize, terminalSearchSummary, terminalViewChord } from "./viewControls";
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
