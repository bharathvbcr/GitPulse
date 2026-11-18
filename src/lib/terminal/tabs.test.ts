import { DEFAULT_TERMINAL_SESSIONS, setTerminalSessionLimit } from "./sessionLimit";
import { describe, expect, it } from "vitest";
import {
  activateTab,
  canOpenTab,
  closeTab,
  createTab,
  cycleTab,
  initialState,
  launcherLabel,
  MAX_STRIP_TABS,
  openTab,
  openAttachedChildTab,
  openFileTab,
  fileTabFor,
  canRepointFileTab,
  keepFileTabFor,
  normalizePaneFilePath,
  tabFor,
  setTabPurpose,
  setTabInitialCommand,
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

  it("keeps the initial prompt on its own agent tab across selection and title changes", () => {
    const shell = initialState();
    const opened = openTab(shell, "codex", "Generate coverage\nfor this checkout");
    const agent = opened.tabs[1];
    const renamed = setTabTitle(opened, agent.id, "Working");
    const selected = activateTab(renamed, shell.activeId!);
    expect(selected.tabs[1].initialPrompt).toBe("Generate coverage\nfor this checkout");
    expect(selected.tabs[0].initialPrompt).toBeUndefined();
    expect(shell.tabs).toHaveLength(1);
  });

  it("refuses to open past the strip's ceiling", () => {
    let state = initialState();
    while (canOpenTab(state, MAX_STRIP_TABS)) state = openTab(state, "shell");
    expect(state.tabs).toHaveLength(MAX_STRIP_TABS);
    // Identity, not just length: "nothing happened" has to be observable.
    expect(openTab(state, "shell")).toBe(state);
  });

  it("does not count ended tabs against the session limit", () => {
    // The session limit bounds live processes and the registry enforces it.
    // The strip used to stop at it too, so a repository whose agents had
    // started and ended as many times as the limit could start no more.
    let state = initialState();
    for (let i = 1; i < DEFAULT_TERMINAL_SESSIONS; i += 1) state = openTab(state, "claude", { runId: `ended-${i}`, title: `Ended ${i}` });
    expect(state.tabs).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
    const next = openTab(state, "claude", { runId: "fresh", title: "Fresh attempt" });
    expect(next.tabs).toHaveLength(DEFAULT_TERMINAL_SESSIONS + 1);
    expect(next.tabs.at(-1)).toMatchObject({ taskRunId: "fresh" });
  });

  it("carries the checkout a hosted launch runs in, and never onto a reattached session", () => {
    const hosted = openTab(initialState(), "claude", { runId: "r", title: "T", checkout: "/repo/.gitpulse/worktrees/a", startDir: "pkg" });
    expect(hosted.tabs.at(-1)).toMatchObject({ checkout: "/repo/.gitpulse/worktrees/a", startDir: "pkg" });
    const own = openTab(initialState(), "claude", { runId: "r", title: "T" });
    expect(own.tabs.at(-1)).not.toHaveProperty("checkout");
    const attached = openTab(initialState(), "shell", { runId: "x", title: "T", attach: { sessionId: "s-1" }, checkout: "/elsewhere" });
    expect(attached.tabs.at(-1)).not.toHaveProperty("checkout");
  });

  it("stops at the user's limit, read when asked", () => {
    try {
      setTerminalSessionLimit(40);
      let state = initialState();
      while (canOpenTab(state)) state = openTab(state, "shell");
      expect(state.tabs).toHaveLength(40);
      // A reactive caller passes the limit it subscribed to.
      expect(canOpenTab(state, 41)).toBe(true);
      setTerminalSessionLimit(3);
      expect(canOpenTab(initialState())).toBe(true);
      expect(canOpenTab(openTab(openTab(initialState(), "shell"), "shell"))).toBe(false);
    } finally {
      setTerminalSessionLimit(undefined);
    }
  });

  it("accepts more sessions than the old 16-process ceiling", () => {
    // The tab strip used to look like a four-tab control and the process
    // budget used to stop at 16. Opening past both is the point of the new
    // ceiling; a test that only filled 16 would still pass on the old cap.
    expect(DEFAULT_TERMINAL_SESSIONS).toBeGreaterThan(16);
    let state = initialState();
    for (let i = 0; i < 20; i += 1) state = openTab(state, "shell");
    expect(state.tabs).toHaveLength(21);
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
      expect(launcherLabel("grok")).toBe("Grok");
      expect(launcherLabel("agy")).toBe("Antigravity");
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

describe("a resumed conversation's tab", () => {
  const SESSION = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
  const resume = { runId: "run-1", title: "Fix the importer", resume: { sessionId: SESSION, mode: "inspect" as const } };

  it("is a plain Claude tab carrying the session and the attempt's mode, not the attempt", () => {
    const state = openTab(initialState(), "claude", resume);
    const tab = state.tabs.find((item) => item.id === state.activeId)!;
    expect(tab.launcher).toBe("claude");
    // It names the attempt it continues (so that task can list it) without
    // becoming that attempt: `taskRunId`, which a close acts on, stays unset.
    expect(tab.resume).toEqual({ sessionId: SESSION, mode: "inspect", runId: "run-1" });
    expect(tab.taskRunId).toBeUndefined();
    expect(tabLabel(tab)).toContain("(resumed)");
  });

  it("is focused rather than opened twice, and never confused with the attempt's own tab", () => {
    const withAttempt = openTab(initialState(), "claude", { runId: "run-1", title: "Fix the importer" });
    const resumed = openTab(withAttempt, "claude", resume);
    expect(resumed.tabs).toHaveLength(withAttempt.tabs.length + 1);
    const again = openTab(openTab(resumed, "shell"), "claude", resume);
    expect(again.tabs).toHaveLength(resumed.tabs.length + 1);
    expect(again.activeId).toBe(resumed.activeId);
    expect(tabFor(again, resume)?.id).toBe(resumed.activeId);
    expect(tabFor(again, { runId: "run-1", title: "x" })?.taskRunId).toBe("run-1");
  });

  it("starts in the conversation's own directory when that is below the checkout root", () => {
    // Claude Code keeps a conversation under the directory it ran in, so a
    // resume started at the checkout root finds no conversation.
    const state = openTab(initialState(), "claude", { ...resume, startDir: "packages/web" });
    expect(state.tabs.find((item) => item.id === state.activeId)?.startDir).toBe("packages/web");
    const attempt = openTab(initialState(), "claude", { runId: "run-2", title: "Fix", startDir: "packages/web" });
    expect(attempt.tabs.find((item) => item.id === attempt.activeId)?.startDir).toBe("packages/web");
    // A directory that climbs or is absolute is one the host refuses; it is
    // not carried.
    for (const startDir of ["../x", "/abs", "a/../../b"]) {
      const opened = openTab(initialState(), "claude", { ...resume, startDir });
      expect(opened.tabs.find((item) => item.id === opened.activeId)?.resume, startDir).toBeDefined();
      expect(opened.tabs.find((item) => item.id === opened.activeId)?.startDir, startDir).toBeUndefined();
    }
  });

  it("opens nothing for a launcher or session id Claude Code would refuse", () => {
    const start = initialState();
    expect(openTab(start, "codex", resume)).toBe(start);
    expect(openTab(start, "claude", { ...resume, resume: { sessionId: "run-1", mode: "edit" as const } })).toBe(start);
  });
});

describe("a tab taking over a session a reload left running", () => {
  const attach = { runId: "detached:term-1-a", title: "Claude", attach: { sessionId: "term-1-a" } };

  it("carries the session to take over and is focused, not duplicated, when asked again", () => {
    const state = openTab(initialState(), "claude", attach);
    const tab = state.tabs.find((item) => item.id === state.activeId)!;
    expect(tab.attachSessionId).toBe("term-1-a");
    expect(tab.taskRunId).toBeUndefined();
    expect(tab.resume).toBeUndefined();
    const again = openTab(openTab(state, "shell"), "claude", attach);
    expect(again.activeId).toBe(state.activeId);
    expect(tabFor(again, attach)?.attachSessionId).toBe("term-1-a");
  });

  it("refuses a launch that would both resume and attach", () => {
    const start = initialState();
    expect(openTab(start, "claude", { ...attach, resume: { sessionId: "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11", mode: "edit" as const } })).toBe(start);
    expect(openTab(start, "claude", { ...attach, attach: { sessionId: "" } })).toBe(start);
  });
});

describe("file pane tabs", () => {
  it("normalizes relative paths and refuses traversal or absolute paths", () => {
    expect(normalizePaneFilePath("./src/lib/main.ts")).toBe("src/lib/main.ts");
    expect(normalizePaneFilePath("src\\lib\\main.ts")).toBe("src/lib/main.ts");
    expect(normalizePaneFilePath("../etc/passwd")).toBeUndefined();
    expect(normalizePaneFilePath("/abs/file.ts")).toBeUndefined();
    expect(normalizePaneFilePath("a/\0/b")).toBeUndefined();
  });

  it("opens a file tab, reuses it across link clicks, and optionally preserves terminal focus", () => {
    const start = initialState();
    const shellId = start.activeId!;
    const first = openFileTab(start, { filePath: "src/App.svelte", line: 42, focus: false });
    expect(first.opened).toBe(true);
    expect(first.state.activeId).toBe(shellId);
    const fileTab = first.state.tabs.find((t) => t.id === first.id)!;
    expect(fileTab.filePath).toBe("src/App.svelte");
    expect(fileTab.revealLine).toBe(42);
    expect(tabLabel(fileTab)).toBe("App.svelte");

    const second = openFileTab(first.state, { filePath: "src/main.ts", line: 10, column: 4, focus: false });
    expect(second.opened).toBe(false);
    expect(second.id).toBe(first.id);
    expect(second.state.tabs).toHaveLength(2);
    const updated = second.state.tabs.find((t) => t.id === second.id)!;
    expect(updated.filePath).toBe("src/main.ts");
    expect(updated.revealLine).toBe(10);
    expect(updated.revealColumn).toBe(4);
    expect((updated.revealSeq ?? 0)).toBeGreaterThan(fileTab.revealSeq ?? 0);
  });

  it("isolates worktree checkouts across file tabs and carries subdir fallbackPath", () => {
    const start = initialState();
    const rootFile = openFileTab(start, {
      filePath: "src-tauri/src-tauri/src/lib.rs",
      fallbackPath: "src-tauri/src/lib.rs",
      line: 18,
      focus: false,
    });
    const rootTab = rootFile.state.tabs.find((t) => t.id === rootFile.id)!;
    expect(rootTab.checkout).toBeUndefined();
    expect(rootTab.fallbackPath).toBe("src-tauri/src/lib.rs");

    // A hosted worktree agent opening a file split gets its own worktree-bound file tab
    // instead of hijacking the root checkout's file tab.
    const wtFile = openFileTab(rootFile.state, {
      filePath: "src/lib/main.ts",
      checkout: "/repo/.gitpulse/worktrees/lane-a",
      line: 7,
      focus: false,
    });
    expect(wtFile.opened).toBe(true);
    expect(wtFile.id).not.toBe(rootFile.id);
    const wtTab = wtFile.state.tabs.find((t) => t.id === wtFile.id)!;
    expect(wtTab.checkout).toBe("/repo/.gitpulse/worktrees/lane-a");
    expect(wtTab.fallbackPath).toBeUndefined();

    // Subsequent link click in the same worktree reuses that worktree's file tab.
    const wtNext = openFileTab(wtFile.state, {
      filePath: "README.md",
      checkout: "/repo/.gitpulse/worktrees/lane-a",
      line: 3,
      focus: false,
    });
    expect(wtNext.opened).toBe(false);
    expect(wtNext.id).toBe(wtFile.id);
  });
});

describe("orchestrated child tabs (openAttachedChildTab)", () => {
  it("adopts a child PTY without stealing focus, deduplicates by sessionId, and sanitizes metadata", () => {
    const start = openTab(initialState(), "claude");
    const parentId = start.activeId!;
    const adopted = openAttachedChildTab(start, {
      sessionId: "term-99-child",
      launcher: "agy",
      title: "Review\u0007Worker",
      startDir: "packages/core",
      focus: false,
    });
    expect(adopted.opened).toBe(true);
    expect(adopted.state.activeId).toBe(parentId);
    const childTab = adopted.state.tabs.find((t) => t.id === adopted.id)!;
    expect(childTab).toMatchObject({
      launcher: "agy",
      attachSessionId: "term-99-child",
      name: "Review Worker",
      startDir: "packages/core",
    });

    const duplicate = openAttachedChildTab(adopted.state, {
      sessionId: "term-99-child",
      launcher: "shell",
      focus: false,
    });
    expect(duplicate.opened).toBe(false);
    expect(duplicate.id).toBe(adopted.id);
    expect(duplicate.state).toBe(adopted.state);

    const focusedDup = openAttachedChildTab(adopted.state, {
      sessionId: "term-99-child",
      focus: true,
    });
    expect(focusedDup.opened).toBe(false);
    expect(focusedDup.state.activeId).toBe(adopted.id);
  });

  it("falls back to shell for unknown launcher kinds, strips unsafe startDir, and stops at MAX_STRIP_TABS", () => {
    const blank = openAttachedChildTab(initialState(), { sessionId: "   " });
    expect(blank.opened).toBe(false);
    expect(blank.id).toBeNull();

    const fallback = openAttachedChildTab(initialState(), {
      sessionId: "term-100-x",
      launcher: "unknown-cli",
      startDir: "../escape",
    });
    const tab = fallback.state.tabs.find((t) => t.id === fallback.id)!;
    expect(tab.launcher).toBe("shell");
    expect(tab.startDir).toBeUndefined();

    let full: TabState = initialState();
    while (canOpenTab(full, MAX_STRIP_TABS)) full = openTab(full, "shell");
    const refused = openAttachedChildTab(full, { sessionId: "term-over-limit" });
    expect(refused.opened).toBe(false);
    expect(refused.id).toBeNull();
    expect(refused.state).toBe(full);
  });

  it("records parentSessionId on attached child tabs, filters live children per parent, and builds pair handoff prompts", async () => {
    const mod = await import("./tabs");
    const base = initialState("claude");
    const adoptedA = openAttachedChildTab(base, {
      sessionId: "term-parent-child-1",
      parentSessionId: "term-parent-1",
      launcher: "shell",
      title: "worker-1",
    });
    const adoptedB = openAttachedChildTab(adoptedA.state, {
      sessionId: "term-parent-child-2",
      parentSessionId: "term-parent-1",
      launcher: "agy",
      title: "worker-2",
    });
    const otherParent = openAttachedChildTab(adoptedB.state, {
      sessionId: "term-other-child",
      parentSessionId: "term-parent-2",
      launcher: "shell",
    });

    const childTabs = mod.childTabsForParent(otherParent.state, "term-parent-1", {
      [adoptedA.id!]: "running",
      [adoptedB.id!]: "exited",
    });
    expect(childTabs.map((t: { id: string }) => t.id)).toEqual([adoptedA.id]);

    const handoff = mod.buildPairHandoffPrompt({
      fromLauncher: "claude",
      openFilePath: "src/lib/app.ts",
      openFileLine: 42,
      terminalTail: "\x1b[32mPASS\x1b[0m src/lib/app.test.ts\nNeeds UI polish in Header\n",
    });
    expect(handoff).toContain("Handoff from Claude");
    expect(handoff).toContain("@src/lib/app.ts#L42");
    expect(handoff).toContain("PASS src/lib/app.test.ts");
    expect(handoff).not.toContain("\x1b[");
  });

  it("validates worktree slugs and opens checkout-bound agent tabs without fake taskRunIds", async () => {
    const mod = await import("./tabs");
    expect(mod.sanitizeWorktreeSlug("  Feature_42 / Fix  ")).toBe("feature_42-fix");
    expect(mod.sanitizeWorktreeSlug("..")).toBeNull();
    expect(mod.sanitizeWorktreeSlug("   ")).toBeNull();

    const wtOpened = mod.openCheckoutTab(initialState("shell"), "claude", {
      checkout: "/Users/dev/repo.wt-agent-1",
      name: "Claude · agent-1",
    });
    expect(wtOpened.id).toBeTruthy();
    const wtTab = wtOpened.state.tabs.find((t: { id: string }) => t.id === wtOpened.id)!;
    expect(wtTab).toMatchObject({
      launcher: "claude",
      checkout: "/Users/dev/repo.wt-agent-1",
      name: "Claude · agent-1",
    });
    expect(wtTab.taskRunId).toBeUndefined();
  });

  it("lets live OSC titles override launch-seeded names while letting manual renames win and clear cleanly", async () => {
    const mod = await import("./tabs");
    // 1. Task-launched tab: starts with task title, then yields primary to live OSC title while keeping launch name as secondary.
    let taskState = openTab(initialState("shell"), "claude", { runId: "run-42", title: "Preserve E42" });
    const taskId = taskState.activeId!;
    expect(tabLabel(taskState.tabs.find((t) => t.id === taskId)!)).toBe("Preserve E42");
    taskState = setTabTitle(taskState, taskId, "✳ Running cargo test");
    const taskUpdated = taskState.tabs.find((t) => t.id === taskId)!;
    expect(tabLabel(taskUpdated)).toBe("✳ Running cargo test");
    expect(mod.tabDisplayTitle(taskUpdated)).toEqual({
      primary: "✳ Running cargo test",
      secondary: "Preserve E42",
      fullTitle: "✳ Running cargo test",
      source: "agent",
    });

    // 2. Worktree checkout-launched tab & resumed tab & attached child tab: live OSC titles supersede launch names.
    const wt = mod.openCheckoutTab(initialState("shell"), "claude", {
      checkout: "/Users/dev/repo.wt-a",
      name: "Claude · wt-a",
    });
    let wtState = setTabTitle(wt.state, wt.id!, "✳ Refactor auth");
    const wtTab = wtState.tabs.find((t) => t.id === wt.id!)!;
    expect(tabLabel(wtTab)).toBe("✳ Refactor auth");
    expect(mod.tabDisplayTitle(wtTab).secondary).toBe("Claude · wt-a");

    const resumed = openTab(initialState("shell"), "claude", {
      runId: "run-9",
      title: "Fix importer",
      resume: { sessionId: "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11", mode: "edit" },
    });
    const resumedLive = setTabTitle(resumed, resumed.activeId!, "✳ Auditing importers");
    const resumedTab = resumedLive.tabs.find((t) => t.id === resumed.activeId!)!;
    expect(tabLabel(resumedTab)).toBe("✳ Auditing importers");
    expect(mod.tabDisplayTitle(resumedTab).secondary).toBe("Fix importer (resumed)");

    const child = openAttachedChildTab(initialState("claude"), {
      sessionId: "term-child-88",
      launcher: "agy",
      title: "Review Worker",
    });
    const childLive = setTabTitle(child.state, child.id!, "Patching split header");
    const childTab = childLive.tabs.find((t) => t.id === child.id!)!;
    expect(tabLabel(childTab)).toBe("Patching split header");
    expect(mod.tabDisplayTitle(childTab).secondary).toBe("Review Worker");

    // 3. Explicit user rename sets userNamed: true, overrides live OSC title, and clearing restores automatic title.
    wtState = mod.renameTab(wtState, wt.id!, "Pinned Name");
    expect(wtState.tabs.find((t) => t.id === wt.id!)?.userNamed).toBe(true);
    wtState = setTabTitle(wtState, wt.id!, "✳ New background title");
    const pinnedTab = wtState.tabs.find((t) => t.id === wt.id!)!;
    expect(tabLabel(pinnedTab)).toBe("Pinned Name");
    expect(mod.tabDisplayTitle(pinnedTab)).toEqual({
      primary: "Pinned Name",
      secondary: "✳ New background title",
      fullTitle: "Pinned Name",
      source: "custom",
    });

    wtState = mod.renameTab(wtState, wt.id!, "   ");
    const unpinnedTab = wtState.tabs.find((t) => t.id === wt.id!)!;
    expect(unpinnedTab.userNamed).toBe(false);
    expect(tabLabel(unpinnedTab)).toBe("✳ New background title");
    expect(mod.tabDisplayTitle(unpinnedTab).secondary).toBe("Claude · wt-a");

    // Clearing a user rename when title is null reverts to launchName cleanly.
    const revertedNoOsc = mod.renameTab(
      mod.renameTab(wt.state, wt.id!, "Temp Name"),
      wt.id!,
      "",
    ).tabs.find((t) => t.id === wt.id!)!;
    expect(revertedNoOsc.userNamed).toBe(false);
    expect(tabLabel(revertedNoOsc)).toBe("Claude · wt-a");
  });

  it("surfaces OSC 7501 status detail in tabDisplayTitle and effectiveSessionTitle without replacing default tabLabel", async () => {
    const mod = await import("./tabs");
    const tab = createTab("claude");
    const agentStatus = { state: "working", detail: "Auditing splitTree.ts" };
    expect(tabLabel(tab)).toBe("Claude");
    expect(mod.tabDisplayTitle(tab, agentStatus)).toEqual({
      primary: "Claude",
      secondary: "Auditing splitTree.ts",
      fullTitle: "Claude",
      source: "launcher",
    });
    expect(mod.effectiveSessionTitle(tab, agentStatus)).toBe("Auditing splitTree.ts");

    const liveTab = { ...tab, title: "✳ Editing TerminalPanel.svelte" };
    expect(mod.effectiveSessionTitle(liveTab, agentStatus)).toBe("✳ Editing TerminalPanel.svelte");
  });

  it("deduplicates high-frequency spinner frames in setTabTitle and coalesces rapid OSC title bursts", async () => {
    const mod = await import("./tabs");
    const base = initialState("claude");
    const id = base.activeId!;
    const first = setTabTitle(base, id, "⠋ Auditing splitTree.ts");
    expect(first.tabs[0].title).toBe("Auditing splitTree.ts");

    // Subsequent Braille and quarter-circle spinner frames normalize identically -> exact reference equality!
    for (const frame of [
      "⠙ Auditing splitTree.ts",
      "⠹ Auditing splitTree.ts",
      "◐ Auditing splitTree.ts",
      "Auditing splitTree.ts ◒",
      "  Auditing splitTree.ts  ",
    ]) {
      expect(setTabTitle(first, id, frame)).toBe(first);
    }

    // Static status icons (✳) are NOT stripped; bare spinner normalizes to null.
    const star = setTabTitle(first, id, "✳ Ready for review");
    expect(star.tabs[0].title).toBe("✳ Ready for review");
    const bare = setTabTitle(star, id, "⠋");
    expect(bare.tabs[0].title).toBeNull();
    expect(setTabTitle(bare, id, "⠙")).toBe(bare);

    // Leading + trailing coalescer: immediate first delivery, coalesced 10 Hz burst, clean dispose.
    let now = 1_000;
    const timers = new Map<number, { fn: () => void; due: number }>();
    let nextTimerId = 1;
    const delivered: string[] = [];
    const coalescer = mod.createTitleCoalescer(
      (title) => delivered.push(title),
      150,
      {
        now: () => now,
        setTimeout: ((fn: () => void, ms?: number) => {
          const tid = nextTimerId++;
          timers.set(tid, { fn, due: now + (ms ?? 0) });
          return tid as unknown as ReturnType<typeof setTimeout>;
        }) as typeof setTimeout,
        clearTimeout: ((tid: unknown) => {
          timers.delete(tid as number);
        }) as typeof clearTimeout,
      },
    );

    coalescer.push("⠋ Phase 1");
    expect(delivered).toEqual(["Phase 1"]);
    now += 80;
    coalescer.push("⠙ Phase 1"); // identical normalized -> no timer needed
    expect(timers.size).toBe(0);

    coalescer.push("⠹ Phase 2");
    now += 30;
    coalescer.push("⠸ Phase 3");
    expect(delivered).toEqual(["Phase 1"]);
    expect(timers.size).toBe(1);

    // Advance past 150ms window -> delivers trailing "Phase 3" once
    now += 150;
    const pending = [...timers.values()][0];
    timers.clear();
    pending.fn();
    expect(delivered).toEqual(["Phase 1", "Phase 3"]);

    // Flush & dispose safety
    now += 20;
    coalescer.push("Phase 4");
    coalescer.flush();
    expect(delivered).toEqual(["Phase 1", "Phase 3", "Phase 4"]);
    now += 20;
    coalescer.push("Phase 5");
    coalescer.dispose();
    expect(timers.size).toBe(0);
    coalescer.push("Phase 6");
    expect(delivered).toEqual(["Phase 1", "Phase 3", "Phase 4"]);
  });

  it("handles browser URL/loading/error titles, file :revealLine context, snippet isolation, and stale userNamed resets", async () => {
    const mod = await import("./tabs");
    expect(mod.browserHostLabel("http://localhost:5173/dashboard?tab=1")).toBe("localhost:5173");
    expect(mod.browserHostLabel("127.0.0.1:8080/api")).toBe("127.0.0.1:8080");
    expect(mod.browserHostLabel("javascript:alert(1)")).toBe("");
    expect(mod.browserHostLabel("   ")).toBe("");

    // Browser tab: host:port fallback before <title>, loading/error states, and live <title> promotion
    const browserOpened = mod.openBrowserTab(initialState("shell"), "http://localhost:5173/login");
    const bTab = browserOpened.state.tabs.find((t) => t.id === browserOpened.id)!;
    expect(tabLabel(bTab)).toBe("localhost:5173");
    expect(mod.tabDisplayTitle(bTab, null, { loading: true })).toEqual({
      primary: "localhost:5173",
      secondary: "Loading…",
      fullTitle: "localhost:5173",
      source: "url",
    });
    expect(mod.tabDisplayTitle(bTab, null, { error: "Connection refused" }).secondary).toBe("Connection refused");
    const bTitled = setTabTitle(browserOpened.state, bTab.id, "GitPulse Preview").tabs.find((t) => t.id === bTab.id)!;
    expect(mod.tabDisplayTitle(bTitled)).toEqual({
      primary: "GitPulse Preview",
      secondary: "localhost:5173",
      fullTitle: "GitPulse Preview",
      source: "agent",
    });

    // Snippet tab: never clobbered by openFileTab / fileTabFor, carries Captured output secondary
    const withSnippet = mod.openSnippetTab(initialState("shell"), {
      title: "cargo test --lib",
      content: "test result: ok. 42 passed",
    });
    const snipTab = withSnippet.state.tabs.find((t) => t.id === withSnippet.id)!;
    expect(mod.tabDisplayTitle(snipTab)).toEqual({
      primary: "cargo test --lib",
      secondary: "Captured output",
      fullTitle: "cargo test --lib",
      source: "file",
    });
    expect(mod.fileTabFor(withSnippet.state)).toBeUndefined();

    const fileAlongside = openFileTab(withSnippet.state, {
      filePath: "src/App.svelte",
      line: 42,
      column: 7,
    });
    expect(fileAlongside.opened).toBe(true);
    expect(fileAlongside.id).not.toBe(withSnippet.id);
    const fTab = fileAlongside.state.tabs.find((t) => t.id === fileAlongside.id)!;
    expect(tabLabel(fTab)).toBe("App.svelte");
    expect(mod.tabDisplayTitle(fTab)).toEqual({
      primary: "App.svelte:42",
      secondary: "src:42",
      fullTitle: "src/App.svelte:42:7",
      source: "file",
    });

    const rootFile = mod.setTabFile(fileAlongside.state, fTab.id, "README.md", 12).tabs.find((t) => t.id === fTab.id)!;
    expect(mod.tabDisplayTitle(rootFile)).toEqual({
      primary: "README.md:12",
      secondary: "L12",
      fullTitle: "README.md:12",
      source: "file",
    });

    // User rename preserved on same-file line jump, reset when switching to a different file or snippet
    const renamedFileState = mod.renameTab(fileAlongside.state, fTab.id, "App Entry");
    const sameFileLine = mod.setTabFile(renamedFileState, fTab.id, "src/App.svelte", 99).tabs.find((t) => t.id === fTab.id)!;
    expect(sameFileLine.userNamed).toBe(true);
    expect(tabLabel(sameFileLine)).toBe("App Entry");

    const switchedFile = mod.setTabFile(renamedFileState, fTab.id, "src/main.ts", 5).tabs.find((t) => t.id === fTab.id)!;
    expect(switchedFile.userNamed).toBe(false);
    expect(tabLabel(switchedFile)).toBe("main.ts");

    const renamedSnipState = mod.renameTab(withSnippet.state, snipTab.id, "Saved Logs");
    const reusedSnip = mod.openSnippetTab(renamedSnipState, {
      title: "vitest run",
      content: "100 passed",
    });
    expect(reusedSnip.opened).toBe(false);
    const updatedSnip = reusedSnip.state.tabs.find((t) => t.id === snipTab.id)!;
    expect(updatedSnip.userNamed).toBe(false);
    expect(tabLabel(updatedSnip)).toBe("vitest run");
  });

  it("toggles split-pane pinning idempotently and resolves multi-agent pair handoff targets with #slot disambiguation", async () => {
    const mod = await import("./tabs");
    let state = initialState("claude");
    const c1 = state.activeId!;
    expect(mod.toggleTabPinned(state, "missing-id")).toBe(state);
    expect(mod.toggleTabPinned(state, c1, false)).toBe(state);
    const pinned = mod.toggleTabPinned(state, c1);
    expect(pinned.tabs[0].pinned).toBe(true);
    expect(mod.toggleTabPinned(pinned, c1, true)).toBe(pinned);
    const unpinned = mod.toggleTabPinned(pinned, c1);
    expect(unpinned.tabs[0].pinned).toBeUndefined();

    // Multi-agent & same-agent pair handoff
    state = openTab(state, "codex");
    const codexId = state.activeId!;
    state = openTab(state, "claude");
    const c2 = state.activeId!;
    state = openTab(state, "shell");
    const shellId = state.activeId!;

    // 2-pane Codex <-> Claude: distinct launchers need no #slot suffix
    expect(mod.resolvePairHandoffTargets(state.tabs, [c1, codexId], c1)).toEqual([
      {
        id: codexId,
        launcher: "codex",
        slot: 2,
        fromSlot: 1,
        label: "Codex",
        buttonLabel: "Handoff -> Codex",
      },
    ]);

    // 2-pane Claude #1 <-> Claude #2: same launcher adds #slot disambiguation
    expect(mod.resolvePairHandoffTargets(state.tabs, [c1, c2], c1)).toEqual([
      {
        id: c2,
        launcher: "claude",
        slot: 2,
        fromSlot: 1,
        label: "Claude #2",
        buttonLabel: "Handoff -> Claude #2",
      },
    ]);

    // 4-pane split with exited peer and non-agent shell: filters shell + exited, numbers visible slots accurately
    const threeWay = mod.resolvePairHandoffTargets(
      state.tabs,
      [c1, shellId, codexId, c2],
      c1,
      { [c1]: "running", [codexId]: "running", [c2]: "exited" },
    );
    expect(threeWay).toEqual([
      {
        id: codexId,
        launcher: "codex",
        slot: 3,
        fromSlot: 1,
        label: "Codex",
        buttonLabel: "Handoff -> Codex",
      },
    ]);

    // Active shell or exited active agent offers no handoff
    expect(mod.resolvePairHandoffTargets(state.tabs, [shellId, codexId], shellId)).toEqual([]);
    expect(mod.resolvePairHandoffTargets(state.tabs, [c1, codexId], c1, { [c1]: "exited" })).toEqual([]);

    const prompt = mod.buildPairHandoffPrompt({
      fromLauncher: "claude",
      fromSlot: 1,
      toLauncher: "claude",
      toSlot: 2,
      checkoutBranch: "feat/split-ux",
    });
    expect(prompt).toBe("[Handoff from Claude #1 -> Claude #2 (feat/split-ux)]\n");
  });

  it("exposes File Explorer in dockTabTypesFor and offeredDockTabKind without polluting process LAUNCHERS or SESSION_TYPES", async () => {
    const mod = await import("./tabs");
    expect(mod.dockTabTypesFor(true).map((t: { kind: string }) => t.kind)).toEqual([
      ...mod.LAUNCHERS.map((l) => l.kind),
      "browser",
      "file",
    ]);
    expect(mod.dockTabTypesFor(false).map((t: { kind: string }) => t.kind)).toEqual([
      ...mod.LAUNCHERS.map((l) => l.kind),
      "file",
    ]);
    expect(mod.dockTabTypesFor(true).find((t: { kind: string }) => t.kind === "file")?.label).toBe(
      "File Explorer",
    );
    expect(mod.offeredDockTabKind("file", false)).toBe("file");
    expect(mod.offeredDockTabKind("browser", false)).toBe("shell");
    expect(mod.offeredDockTabKind("browser", true)).toBe("browser");
    expect(mod.offeredDockTabKind("bogus", true)).toBe("shell");

    // Generic startup OSC titles never clobber task/worktree launchName or duplicate brand labels
    expect(mod.isGenericLauncherTitle("✳ Claude Code", "claude")).toBe(true);
    expect(mod.isGenericLauncherTitle("Claude Code", "claude")).toBe(true);
    expect(mod.isGenericLauncherTitle("OpenAI Codex", "codex")).toBe(true);
    expect(mod.isGenericLauncherTitle("✳ Running cargo test", "claude")).toBe(false);

    let seeded = openTab(initialState("shell"), "claude", { runId: "run-77", title: "Fix sidebar nav" });
    const seededId = seeded.activeId!;
    seeded = setTabTitle(seeded, seededId, "✳ Claude Code");
    const seededTab = seeded.tabs.find((t) => t.id === seededId)!;
    expect(tabLabel(seededTab)).toBe("Fix sidebar nav");
    expect(mod.effectiveSessionTitle(seededTab)).toBe("Fix sidebar nav");

    // Astral-plane emoji straddling codepoint 27 in tabLabel never splits surrogate pairs
    const emojiState = setTabTitle(initialState("claude"), "tab-1", `${"a".repeat(26)}🚀!!`);
    const emojiTab = emojiState.tabs[0];
    if (emojiTab) {
      const exactFit = tabLabel({ ...emojiTab, title: `${"a".repeat(26)}🚀!` });
      expect(exactFit.isWellFormed()).toBe(true);
      expect(exactFit).toBe(`${"a".repeat(26)}🚀!`);
      const clipped = tabLabel({ ...emojiTab, title: `${"a".repeat(26)}🚀!!` });
      expect(clipped.isWellFormed()).toBe(true);
      expect(clipped).toBe(`${"a".repeat(26)}🚀…`);
    }
  });
});

describe("setTabPurpose", () => {
  it("marks one tab as a commit launch and ignores an unknown id", () => {
    const opened = openTab(initialState(), "claude");
    const id = opened.activeId!;
    expect(setTabPurpose(opened, id, "commit").tabs.find((t) => t.id === id)?.purpose).toBe("commit");
    expect(setTabPurpose(opened, "missing", "commit")).toBe(opened);
    expect(opened.tabs.find((t) => t.id === id)?.purpose).toBeUndefined();
  });
});

describe("setTabInitialCommand", () => {
  it("seeds a trimmed one-shot command on a shell tab and ignores blank or unknown ids", () => {
    const opened = openTab(initialState(), "shell");
    const id = opened.activeId!;
    expect(setTabInitialCommand(opened, id, "  cargo test --lib  ").tabs.find((t) => t.id === id)?.initialCommand).toBe(
      "cargo test --lib",
    );
    expect(setTabInitialCommand(opened, id, "   ")).toBe(opened);
    expect(setTabInitialCommand(opened, id, null)).toBe(opened);
    expect(setTabInitialCommand(opened, "missing", "ls")).toBe(opened);
    expect(opened.tabs.find((t) => t.id === id)?.initialCommand).toBeUndefined();
  });
});

describe("kept file tabs (a double-click keeps the file)", () => {
  const twoFiles = () => {
    const a = openFileTab(initialState("shell"), { filePath: "src/a.ts" });
    const b = openFileTab(a.state, { filePath: "src/b.ts", reuseExisting: false });
    return { state: b.state, aId: a.id!, bId: b.id! };
  };

  it("keeps the tab showing the file, preferring the named one", () => {
    const { state, aId } = twoFiles();
    const kept = keepFileTabFor(state, "./src/a.ts", undefined, aId);
    expect(kept.id).toBe(aId);
    expect(kept.state.tabs.find((t) => t.id === aId)?.kept).toBe(true);
    // Keeping again changes nothing.
    expect(keepFileTabFor(kept.state, "src/a.ts").state).toBe(kept.state);
  });

  it("keeps nothing when no tab shows the file, or the path is unusable", () => {
    const { state } = twoFiles();
    expect(keepFileTabFor(state, "src/c.ts")).toEqual({ state, id: null });
    expect(keepFileTabFor(state, "../escape.ts")).toEqual({ state, id: null });
  });

  it("never hands a kept tab out to be repointed at another file", () => {
    const a = openFileTab(initialState("shell"), { filePath: "src/a.ts" });
    const kept = keepFileTabFor(a.state, "src/a.ts").state;
    expect(fileTabFor(kept, "src/b.ts")).toBeUndefined();
    // The file it shows still finds it.
    expect(fileTabFor(kept, "src/a.ts")?.id).toBe(a.id);
    // So a single click elsewhere opens a tab of its own beside it.
    const next = openFileTab(kept, { filePath: "src/b.ts" });
    expect(next.opened).toBe(true);
    expect(next.id).not.toBe(a.id);
    expect(next.state.tabs.find((t) => t.id === a.id)?.filePath).toBe("src/a.ts");
  });

  it("lets a preview tab be repointed, and a kept one only to its own file", () => {
    const a = openFileTab(initialState("shell"), { filePath: "src/a.ts" });
    const tab = a.state.tabs.find((t) => t.id === a.id)!;
    expect(canRepointFileTab(tab, "src/b.ts")).toBe(true);
    const keptTab = { ...tab, kept: true };
    expect(canRepointFileTab(keptTab, "src/b.ts")).toBe(false);
    expect(canRepointFileTab(keptTab, "./src/a.ts")).toBe(true);
    expect(canRepointFileTab(keptTab, undefined)).toBe(false);
  });

  it("reuses the preview tab beside a kept one on the next single click", () => {
    const { state, aId, bId } = twoFiles();
    const kept = keepFileTabFor(state, "src/a.ts", undefined, aId).state;
    const next = openFileTab(kept, { filePath: "src/c.ts" });
    expect(next.id).toBe(bId);
    expect(next.opened).toBe(false);
    expect(next.state.tabs.find((t) => t.id === aId)?.filePath).toBe("src/a.ts");
  });
});
