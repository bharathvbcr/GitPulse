import { describe, expect, it } from "vitest";
import {
  MAX_TERMINAL_TABS,
  activateTab,
  canOpenTab,
  closeTab,
  createTab,
  cycleTab,
  initialState,
  launcherLabel,
  openTab,
  setTabTitle,
  tabLabel,
  terminalTabChord,
  type TabState,
} from "./tabs";

const chord = (key: string, mods: Partial<Record<"ctrlKey" | "metaKey" | "altKey" | "shiftKey", boolean>> = {}) => ({
  key,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});

describe("terminal tab model", () => {
  it("initializes a task terminal without also opening a shell", () => {
    const state = initialState("codex", { runId: "run-1", title: "Preserve E42" });
    expect(state.tabs).toHaveLength(1);
    expect(state.tabs[0]).toMatchObject({ launcher: "codex", taskRunId: "run-1", name: "Preserve E42" });
    expect(state.activeId).toBe(state.tabs[0].id);
  });
  it("focuses a repeated task even at capacity and keeps distinct attempts separate", () => {
    let state = openTab(initialState(), "codex", { runId: "a", title: "Task A" });
    const first = state.activeId;
    state = openTab(state, "codex", { runId: "b", title: "Task B" });
    expect(state.activeId).not.toBe(first);
    while (canOpenTab(state)) state = openTab(state, "shell");
    const repeated = openTab(state, "codex", { runId: "a", title: "Task A" });
    expect(repeated.tabs).toBe(state.tabs);
    expect(repeated.activeId).toBe(first);
  });
  it("starts with exactly one focused shell", () => {
    const state = initialState();
    expect(state.tabs).toHaveLength(1);
    expect(state.tabs[0].launcher).toBe("shell");
    expect(state.activeId).toBe(state.tabs[0].id);
  });

  it("gives every tab a distinct id, so a keyed each never reuses a session", () => {
    const ids = new Set(Array.from({ length: 50 }, () => createTab("shell").id));
    expect(ids.size).toBe(50);
  });

  it("focuses each newly opened tab", () => {
    let state = initialState();
    const first = state.activeId;
    state = openTab(state, "claude");
    expect(state.tabs).toHaveLength(2);
    expect(state.activeId).not.toBe(first);
    expect(state.tabs[1].launcher).toBe("claude");
  });

  it("refuses to open past the backend's session ceiling", () => {
    let state = initialState();
    while (canOpenTab(state)) state = openTab(state, "shell");
    expect(state.tabs).toHaveLength(MAX_TERMINAL_TABS);
    // Identity, not just length: "nothing happened" has to be observable.
    expect(openTab(state, "shell")).toBe(state);
  });

  describe("closing", () => {
    const four = (): TabState => {
      let state = initialState();
      for (const kind of ["claude", "manvi", "codex"] as const) state = openTab(state, kind);
      return state;
    };

    it("moves focus to the tab on the right", () => {
      let state = four();
      state = activateTab(state, state.tabs[1].id);
      const rightNeighbour = state.tabs[2].id;
      state = closeTab(state, state.tabs[1].id);
      expect(state.activeId).toBe(rightNeighbour);
    });

    it("falls back to the left when the closed tab was last", () => {
      let state = four();
      const last = state.tabs[3].id;
      const leftNeighbour = state.tabs[2].id;
      state = activateTab(state, last);
      state = closeTab(state, last);
      expect(state.activeId).toBe(leftNeighbour);
    });

    it("leaves focus alone when an inactive tab closes", () => {
      let state = four();
      const focused = state.tabs[3].id;
      state = activateTab(state, focused);
      state = closeTab(state, state.tabs[0].id);
      expect(state.activeId).toBe(focused);
      expect(state.tabs).toHaveLength(3);
    });

    it("empties rather than inventing a replacement session", () => {
      // A terminal that silently respawns what you just closed starts a
      // process nobody asked for; the panel offers a new tab instead.
      let state = initialState();
      state = closeTab(state, state.tabs[0].id);
      expect(state.tabs).toHaveLength(0);
      expect(state.activeId).toBeNull();
    });

    it("ignores an unknown id", () => {
      const state = four();
      expect(closeTab(state, "tab-does-not-exist")).toBe(state);
    });
  });

  describe("cycling", () => {
    const three = (): TabState => {
      let state = initialState();
      state = openTab(state, "claude");
      state = openTab(state, "codex");
      return activateTab(state, state.tabs[0].id);
    };

    it("wraps at both ends so the chord never dies silently", () => {
      let state = three();
      state = cycleTab(state, -1);
      expect(state.activeId).toBe(state.tabs[2].id);
      state = cycleTab(state, 1);
      expect(state.activeId).toBe(state.tabs[0].id);
    });

    it("is a no-op below two tabs", () => {
      const state = initialState();
      expect(cycleTab(state, 1)).toBe(state);
    });
  });

  describe("labels", () => {
    it("names a tab after its launcher until the program says otherwise", () => {
      const tab = createTab("manvi");
      expect(tabLabel(tab)).toBe("Manvi");
      expect(launcherLabel("codex")).toBe("Codex");
    });

    it("prefers what the running program calls itself", () => {
      let state = initialState();
      state = setTabTitle(state, state.tabs[0].id, "~/Code/GitPulse");
      expect(tabLabel(state.tabs[0])).toBe("~/Code/GitPulse");
    });

    it("clips a long title instead of letting it stretch the strip", () => {
      let state = initialState();
      state = setTabTitle(state, state.tabs[0].id, "x".repeat(200));
      const label = tabLabel(state.tabs[0]);
      expect(label).toHaveLength(28);
      expect(label.endsWith("…")).toBe(true);
    });

    it("falls back when a program reports only whitespace", () => {
      let state = initialState();
      state = setTabTitle(state, state.tabs[0].id, "   ");
      expect(tabLabel(state.tabs[0])).toBe("Shell");
    });

    it("ignores a title for a tab that has already closed", () => {
      const state = initialState();
      expect(setTabTitle(state, "tab-gone", "hi")).toBe(state);
    });
  });

  describe("keyboard chords", () => {
    it("leaves IME composition untouched and consumes destructive repeats", () => {
      expect(terminalTabChord({ ...chord("W", { ctrlKey: true, shiftKey: true }), isComposing: true })).toBeNull();
      expect(terminalTabChord({ ...chord("T", { ctrlKey: true, shiftKey: true }), keyCode: 229 })).toBeNull();
      expect(terminalTabChord({ ...chord("W", { ctrlKey: true, shiftKey: true }), repeat: true })).toBe("ignore");
    });

    it("recognises the four bindings", () => {
      expect(terminalTabChord(chord("T", { ctrlKey: true, shiftKey: true }))).toBe("new");
      expect(terminalTabChord(chord("W", { ctrlKey: true, shiftKey: true }))).toBe("close");
      expect(terminalTabChord(chord("Tab", { ctrlKey: true }))).toBe("next");
      expect(terminalTabChord(chord("Tab", { ctrlKey: true, shiftKey: true }))).toBe("prev");
    });

    it("never claims a keystroke the shell needs", () => {
      // Everything here must reach the PTY: bare letters, ^C, ^W (delete
      // word), a plain Tab (completion), and every Command/Option chord.
      for (const event of [
        chord("t"),
        chord("Tab"),
        chord("c", { ctrlKey: true }),
        chord("w", { ctrlKey: true }),
        chord("t", { metaKey: true, shiftKey: true }),
        chord("t", { ctrlKey: true, altKey: true, shiftKey: true }),
        chord("t", { ctrlKey: true, metaKey: true, shiftKey: true }),
        chord("q", { ctrlKey: true, shiftKey: true }),
      ]) {
        expect(terminalTabChord(event)).toBeNull();
      }
    });
  });
});


it("bounds stored titles and consumes repeated tab chords without shell input", () => {
  const state = initialState();
  const next = setTabTitle(state, state.tabs[0].id, "x".repeat(100000));
  expect(next.tabs[0].title?.length).toBeLessThanOrEqual(256);
  expect(terminalTabChord({key:"W", ctrlKey:true, metaKey:false, shiftKey:true, altKey:false, repeat:true})).toBe("ignore");
});
