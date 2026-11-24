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
    expect(body).toContain("Dock");
    expect(body).toContain("rounded-t-lg");
    expect(body).not.toContain("rounded-b-lg");
  });

  // Rendered in harness/terminal.html: "hide/show preserves both sessions";
  // "switching repositories spawns nothing", "the other repository's dock is
  // hidden", "each repository's strip holds only its own tabs", "returning
  // restores both original tabs"; "Go to switches to that repository without
  // spawning anything new" and "Go to leaves its dock showing"; "the dock's
  // resize separator grows on ArrowUp and shrinks back on ArrowDown,
  // reporting its value"; expand and restore of the dock height.
  it("hosts a repository's panel, unseen, while a task terminal waits for it", () => {
    // A launch from a task sheet must start its agent without opening the
    // dock or showing the repository; the queued request is what asks for
    // the panel. Matched by the same identity rule the panel consumes with.
    expect(source).toContain("awaitedTabIds($repoStore.openTabs, $taskTerminalRequests, { caseInsensitive: isCaseInsensitiveFs() })");
    expect(source).toMatch(/nextHostedTerminals\(\s*hostedIds,[\s\S]*?open,\s*awaited,?\s*\)/);
    const body = render(TerminalDock, { props: { open: false, onClose: () => {}, load: loader } }).body;
    expect(body).not.toContain("data-terminal-dock");
  });

  it("keys each hosted panel on its repository tab, so a close cannot recycle a shell", () => {
    // The fixture never closes a repository tab whose panel holds a shell.
    expect(source).toContain("{#each hostedTabs as tab (tab.id)}");
  });
  it("jumps to another repository's session through the shared focus owner", () => {
    // The alert bridge uses the same owner (below), so the two jumps cannot
    // drift into different steps.
    expect(source).toContain("return focusTerminalSession(session);");
  });
  it("hands the panel the open tabs, so the Sessions list can name each checkout", () => {
    // The panel cannot read the store; the repository, checkout and agent
    // worktree a session runs in come from the tab on that checkout.
    expect(source).toContain("checkouts: $repoStore.openTabs");
  });

  it("routes isolated agent worktree creation through repoStore.addWorktree with the panel's MutationTarget", () => {
    expect(source).toContain("repoStore.addWorktree(");
    expect(source).toContain("addAgentWorktree({ tabId: tab.id, path: tab.path }, targetPath, newBranch)");
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
    // The capture / move / end / cancel plumbing and the clamp are ui/paneHeight's
    // (its test pins them); the dock's part is what the callbacks do with a height.
    expect(drag).toContain("startPaneDrag(event, startHeight, TERMINAL_DOCK_BOUNDS, {");
    const move = drag.slice(drag.indexOf("onHeight:"), drag.indexOf("onEnd:"));
    expect(move).toContain("dragHeight = px");
    expect(move).not.toContain("interfaceStore.");
    const end = drag.slice(drag.indexOf("onEnd:"));
    expect(end).toContain("commitDragHeight();");
    // The rendered height follows the pending drag value while one exists.
    expect(source).toContain("fitTerminalDockHeight(dragHeight, containerHeight)");
    // Keyboard nudges still write straight through.
    const keys = source.slice(stop);
    expect(keys).toContain("nudgedPaneHeight(event.key, currentHeight, TERMINAL_DOCK_RESIZE_STEP)");
    expect(keys).toContain("interfaceStore.setTerminalDockHeight(next)");
  });
});

describe("TerminalDock modes", () => {
  it("renders docked by default, in its own row, with a sheet that slides up inside it", () => {
    const body = render(TerminalDock, {
      props: { open: true, onClose: () => {}, load: loader },
    }).body;
    expect(body).toContain('data-dock-mode="docked"');
    expect(body).toContain("shrink-0");
    expect(body).not.toContain("absolute inset-x-0 bottom-0");
    // The motion lives on the inner sheet, not the row that holds the height.
    const sheet = body.slice(body.lastIndexOf("<", body.indexOf("data-dock-sheet")), body.indexOf(">", body.indexOf("data-dock-sheet")) + 1);
    expect(sheet).toContain("gp-dock-motion");
    expect(sheet).toContain('data-phase="slide-in"');
    expect(sheet).toContain("bg-surfaceHover");
    const row = body.slice(body.lastIndexOf("<", body.indexOf("data-terminal-dock")), body.indexOf(">", body.indexOf("data-terminal-dock")) + 1);
    expect(row).not.toContain("gp-dock-motion");
    expect(row).not.toContain("data-phase");
  });

  it("floats over the view in overlay mode, held for its exit and then hidden", () => {
    expect(source).toContain("{overlay ? 'absolute inset-x-0 bottom-0' : 'shrink-0'}");
    expect(source).toContain('const phase = $derived(leaving ? "slide-out" : "slide-in");');
    expect(source).toContain("class:hidden={!open && !leaving}");
    // The dock's exit is the `fast` duration, not the 70 ms backdrop-safe exit cap.
    expect(source).toContain('motionMs("fast")');
    expect(source).not.toContain("motionExitMs(");
    // Floating, the shadow is the sheet's, and nothing clips it.
    expect(source).toContain("{overlay ? 'shadow-float' : ''}");
    expect(source).not.toMatch(/data-terminal-dock[\s\S]{0,40}overflow-hidden/);
    // Never unmounted: hiding is display:none, so the PTY survives the exit.
    expect(source).toContain("{#if mounted}");
  });

  it("resumes an interrupted slide from where the sheet is, read before the DOM changes", () => {
    // $effect.pre: once `open` flips, the sheet can already be display:none.
    const pre = source.slice(source.indexOf("$effect.pre("), source.indexOf("$effect(() => {\n    const isOpen = open;"));
    expect(pre).toContain("pendingSlideFrom = inFlightOffset();");
    expect(source).toContain('animation.playState === "running"');
    expect(source).toContain("slideFrom = pendingSlideFrom;");
    expect(source).toContain("style:--gp-dock-from={slideFrom === null ? undefined : `${slideFrom}px`}");
    expect(source).toContain("bind:this={sheet}");
  });

  it("gives focus back when it closes, never to its own pill, and never leaves it in the hidden dock", () => {
    const pre = source.slice(source.indexOf("$effect.pre("), source.indexOf("$effect(() => {\n    const isOpen = open;"));
    expect(pre).toContain("!host?.contains(active) && !active.closest(DOCK_TRIGGER)");
    expect(pre).toContain("target.focus({ preventScroll: true });");
    expect(pre).toContain("active.blur();");
  });

  it("leaves with the exit token when docked too, not only when floating", () => {
    expect(source).toContain("if (isOpen || exitMs === 0) {");
    expect(source).not.toMatch(/isOpen \|\| !overlay/);
    expect(source).toContain("--gp-dock-exit-ms: {exitMs}ms;");
  });

  it("dismisses on an outside pointerdown only while floating and open, with its status-bar trigger inside", () => {
    expect(source).toContain("use:popover={dockDismissOptions}");
    expect(source).toContain('pointer: overlay && open && !leaving ? "pointerdown" : "none"');
    expect(source).toContain('const DOCK_TRIGGER = "[data-terminal-pill], [data-terminal-pill-menu]";');
    expect(source).toContain('floatingPanelDismiss("[data-terminal-dock]", DOCK_TRIGGER)');
  });

  it("hands the panel the mode and a toggle, and keeps its separator and testids", () => {
    expect(source).toContain('onToggleDockMode: () => panelModes.toggle("dock")');
    expect(source).toContain('aria-label="Resize the dock"');
    expect(source).toContain("data-terminal-host={tab.id}");
    expect(source).toContain("data-terminal-dock");
  });
});

describe("App hosts the terminal as a dock, not a view", () => {
  it("renders the dock inside the view column", () => {
    expect(app).toContain("<TerminalDock");
    expect(app).toContain("open={terminalDockOpen}");
  });

  it("hides the view, and takes it out of the tab order, while an open dock is expanded", () => {
    // The expanded dock leaves the view zero height. A control positioned
    // inside that box still paints over the terminal and stays in the tab
    // order unless the view itself is hidden. harness/terminal.html covers
    // expand and restore of the dock.
    expect(source).toContain("expanded = $bindable(false)");
    expect(source).not.toContain("let expanded = $state(");
    expect(app).toContain("bind:expanded={terminalDockExpanded}");
    expect(app).toContain("const viewCoveredByTerminal = $derived(terminalDockOpen && terminalDockExpanded);");
    expect(app).toMatch(
      /<div class="gp-view[^"]*" style:visibility=\{viewCoveredByTerminal \? "hidden" : null\} inert=\{viewCoveredByTerminal\}>/,
    );
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

  it("hides on a double-click on the page or its tab bar, heard before the page can stop it", () => {
    // vitest never runs $effect, so this pins the wiring; the judgement of
    // which targets count is panelDismiss.test.ts's, and the real double-click
    // is harness/terminal.html's "double-click on the page hides the dock".
    expect(source).toContain(
      'const scope = { panel: "[data-terminal-dock]", trigger: DOCK_TRIGGER, column, page: "[data-viewbar]" };',
    );
    expect(source).toMatch(
      /if \(isWindowDragDoubleClick\(event\.composedPath\(\)\)\) return;\s*if \(pageDoubleClickHidesPanel\(event\.target, scope\)\) onClose\(\);/,
    );
    expect(source).toContain('document.addEventListener("dblclick", onDoubleClick, true);');
    expect(source).toContain('document.removeEventListener("dblclick", onDoubleClick, true);');
    expect(source).toMatch(/if \(!column \|\| !open\) return;\s*const scope/);
    // The tab bar the scope names is the view bar's root.
    const viewBarTag = /<nav\b[^>]*\sdata-viewbar(?:\s|>)/;
    expect(readFileSync(join(here, "ViewBar.svelte"), "utf8")).toMatch(viewBarTag);
    expect(viewBarTag.test(`<nav\n${"\n".repeat(5000)}class="x">`)).toBe(false);
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
