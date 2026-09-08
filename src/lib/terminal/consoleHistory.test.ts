import { describe, expect, it } from "vitest";
import { boundedCommand, followsConsoleOutput, retainCommand, retainExecutions } from "./consoleHistory";
import { moveTab, renameTab, initialState, openTab, setTabTitle, tabLabel } from "./tabs";

describe("terminal retained state bounds", () => {
  it("bounds commands by UTF-8 bytes before recording or executing them", () => {
    expect(boundedCommand("x".repeat(65536))).toBe(true);
    expect(boundedCommand("界".repeat(30000))).toBe(false);
    expect(boundedCommand("x".repeat(65537))).toBe(false);
  });
  it("retains the latest 100 commands through 10000 submissions", () => {
    let history: string[] = [];
    for (let i = 0; i < 10000; i++) history = retainCommand(history, String(i));
    expect(history).toHaveLength(100); expect(history[0]).toBe("9900");
    expect(retainCommand(history, "9999")).toBe(history);
  });
  it("bounds both entry count and retained byte size", () => {
    const entries = Array.from({length: 10000}, (_, i) => ({command: String(i)}));
    expect(retainExecutions(entries)).toEqual(entries.slice(-100));
    const large = Array.from({length:100}, () => ({command: "x".repeat(131072)}));
    expect(retainExecutions(large)).toHaveLength(64);
  });
  it("only follows output while the reader is at the bottom", () => {
    expect(followsConsoleOutput(600, 400, 1000)).toBe(true);
    expect(followsConsoleOutput(10, 400, 1000)).toBe(false);
  });
  it("renames without allowing OSC titles to replace the user name", () => {
    const state = initialState();
    const named = renameTab(state, state.activeId!, "Build");
    expect(tabLabel(setTabTitle(named, state.activeId!, "automatic").tabs[0])).toBe("Build");
    expect(tabLabel(renameTab(named, state.activeId!, "").tabs[0])).toBe("Shell");
  });
  it("reorders without changing identity, active selection, or session count", () => {
    const state = openTab(initialState(), "codex");
    const moved = moveTab(state, state.activeId!, -1);
    expect(moved.activeId).toBe(state.activeId);
    expect(moved.tabs).toEqual([...state.tabs].reverse());
    expect(moveTab(moved, "missing", 1)).toBe(moved);
    expect(moveTab(moved, moved.tabs[0].id, -1)).toBe(moved);
  });
});
