import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalDock from "./TerminalDock.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "TerminalDock.svelte"), "utf8");
const app = readFileSync(join(here, "..", "..", "App.svelte"), "utf8");

const loader = () => Promise.resolve({ default: (() => {}) as never });

describe("TerminalDock", () => {
  it("mounts nothing until it is first opened", () => {
    // The dock carries a PTY and the 334 KB xterm runtime. A user who never
    // opens it must never pay for it.
    const body = render(TerminalDock, {
      props: { open: false, onClose: () => {}, load: loader },
    }).body;
    expect(body).not.toContain("data-terminal-dock");
  });

  it("renders the dock once open", () => {
    const body = render(TerminalDock, {
      props: { open: true, onClose: () => {}, load: loader },
    }).body;
    expect(body).toContain("data-terminal-dock");
    expect(body).toContain("Terminal");
  });

  it("hides rather than unmounts, because closing must not kill the shell", () => {
    // The whole reason the terminal could never really be a view: unmounting
    // the pane ends the process. `mounted` latches true; hiding, a view
    // switch, and a repository tab switch all keep the component alive.
    expect(source).toContain("let mounted = $state(open)");
    expect(source).toContain("if (open || awaited.size > 0) mounted = true;");
    expect(source).toContain("class:hidden={!open}");
  });

  it("hosts a repository's panel, unseen, while a task terminal waits for it", () => {
    // A launch from a task sheet must start its agent without opening the
    // dock or showing the repository; the queued request is what asks for
    // the panel. Matched by the same identity rule the panel consumes with.
    expect(source).toContain("awaitedTabIds($repoStore.openTabs, $taskTerminalRequests, { caseInsensitive: isCaseInsensitiveFs() })");
    expect(source).toMatch(/nextHostedTerminals\(\s*hostedIds,[\s\S]*?open,\s*awaited,\s*\)/);
    const body = render(TerminalDock, { props: { open: false, onClose: () => {}, load: loader } }).body;
    expect(body).not.toContain("data-terminal-dock");
  });

  it("keeps one panel per visited repository tab, keyed so a close cannot recycle a shell", () => {
    expect(source).toContain("nextHostedTerminals");
    expect(source).toContain("{#each hostedTabs as tab (tab.id)}");
    expect(source).toContain("data-terminal-host={tab.id}");
    expect(source).toContain("repoPath: tab.path");
    expect(source).toContain("visible: open && tab.id === $repoStore.activeTabId");
  });

  it("owns cross-repository session focus, because the panel may not", () => {
    // TerminalPanel is bound to the single path it was handed and is barred
    // from importing repoStore at all (TerminalPanel.test.ts holds that).
    // Switching repository tabs to reveal a shell therefore has to live here,
    // where the dock already spans every open repository.
    expect(source).toContain("focusTerminalSession");
    expect(source).toContain("onGoToSession: goToSession");
    // The store actions are sessionFocus.ts's own default, shared with the
    // alert bridge, so the two jumps cannot drift into different steps.
    expect(source).toContain("return focusTerminalSession(session);");
    const focus = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../terminal/sessionFocus.ts"), "utf8");
    expect(focus).toContain("setTerminalOpen: (open) => repoStore.setTerminalOpen(open)");
    expect(focus).toContain('showRepositorySurface: () => interfaceStore.setGlobalSurface("repository")');
  });

  it("hands the panel the open tabs, so the Sessions list can name each checkout", () => {
    // The panel cannot read the store; the repository, checkout and agent
    // worktree a session runs in come from the tab on that checkout.
    expect(source).toContain("checkouts: $repoStore.openTabs");
  });

  it("a clicked session alert jumps through the same owner as Go to", () => {
    // It called `record.reveal()` directly: the tab was selected inside
    // whichever repository's panel held it, hidden if that was not the
    // active one, and an adopted session's tab was queued for a dock nobody
    // opened.
    const bridge = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "SessionNotificationBridge.svelte"), "utf8");
    expect(bridge).toContain("await focusTerminalSession(record)");
    expect(bridge).not.toMatch(/record\.reveal\(\)/);
  });

  it("offers the WAI-ARIA splitter, keyboard included", () => {
    expect(source).toContain('role="separator"');
    expect(source).toContain('aria-valuenow={height}');
    expect(source).toContain('event.key === "ArrowUp"');
    expect(source).toContain('event.key === "ArrowDown"');
  });

  it("sizes itself through the clamp rather than trusting the stored height", () => {
    expect(source).toContain("fitTerminalDockHeight($interfaceStore.terminalDockHeight");
    expect(source).toContain("fitTerminalDockHeight(TERMINAL_DOCK_MAX_HEIGHT, containerHeight, 0)");
    expect(source).toContain("aria-valuemax={heightCeiling}");
  });

  it("commits a drag to the store once on release, not on every pointermove", () => {
    // setTerminalDockHeight is a synchronous localStorage write plus an
    // interfaceStore publish; per pointermove it re-ran every subscriber.
    const start = source.indexOf("function startDrag(");
    const stop = source.indexOf("function handleSeparatorKey(");
    expect(start).toBeGreaterThan(-1);
    const drag = source.slice(start, stop);
    const move = drag.slice(drag.indexOf("const move = "), drag.indexOf("const end = "));
    expect(move).toContain("dragHeight = clampTerminalDockHeight(");
    expect(move).not.toContain("interfaceStore.");
    const end = drag.slice(drag.indexOf("const end = "));
    expect(end).toContain("commitDragHeight();");
    expect(drag).toContain('handle.addEventListener("pointerup", end);');
    expect(drag).toContain('handle.addEventListener("pointercancel", end);');
    // The rendered height follows the pending drag value while one exists.
    expect(source).toContain("fitTerminalDockHeight(dragHeight, containerHeight)");
    // Keyboard nudges still write straight through.
    const keys = source.slice(stop);
    expect(keys).toContain("interfaceStore.setTerminalDockHeight(currentHeight + TERMINAL_DOCK_RESIZE_STEP)");
    expect(keys).toContain("interfaceStore.setTerminalDockHeight(currentHeight - TERMINAL_DOCK_RESIZE_STEP)");
  });
});

describe("App hosts the terminal as a dock, not a view", () => {
  it("renders the dock inside the view column", () => {
    expect(app).toContain("<TerminalDock");
    expect(app).toContain("open={terminalDockOpen}");
  });

  it("no longer swaps the main pane out for a terminal", () => {
    // The old shape hid <main> whenever the terminal tab was active, so the
    // shell replaced whatever you were reading.
    expect(app).not.toContain('activeTab === "terminal"');
    expect(app).not.toContain("class:hidden={terminalActive}");
    expect(app).not.toContain("terminalMounted");
  });

  it("binds the chord every terminal-hosting editor uses", () => {
    // Control, not Command, on macOS too: ⌘` is the OS window cycler.
    expect(app).toContain('e.key === "`"');
    expect(app).toContain("repoStore.toggleTerminal()");
  });

  it("reads the dock's open state from the repository tab, not a workspace preference", () => {
    // A single workspace-wide flag meant opening a shell in one repository
    // opened the dock over every other repository the user switched to, and
    // — because hosting a panel starts a shell — spawned a PTY in each.
    expect(app).toContain("$derived($repoStore.terminalOpen)");
    expect(app).not.toContain("interfaceStore.terminalDockOpen");
    expect(app).not.toContain("interfaceStore.setTerminalDockOpen");
  });
});
