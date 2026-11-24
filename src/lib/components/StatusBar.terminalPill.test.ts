/**
 * The terminal control is a pill, not the old 10px muted label. It lives in
 * TerminalPill.svelte (the view bar), no longer in the status bar.
 * Server render covers the closed and open faces. The hover menu is closed
 * until pointer or focus, so its actions are pinned from the component source
 * the same way the rest of StatusBar's wiring is.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import StatusBar from "./StatusBar.svelte";
import TerminalPill from "./TerminalPill.svelte";
import { interfaceStore } from "../stores/interfaceStore";
import { terminalSessions } from "../terminal/sessionRegistry";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalPill.svelte"),
  "utf8",
);
const menuSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalPillMenu.svelte"),
  "utf8",
);
const statusBarSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "StatusBar.svelte"),
  "utf8",
);

const pill = vi.hoisted(() => ({ terminalOpen: false, currentPath: "/repo/app" as string | null }));

vi.mock("../stores/repoStore", async (importOriginal) => {
  const real = await importOriginal<typeof import("../stores/repoStore")>();
  return {
    ...real,
    repoStore: {
      ...real.repoStore,
      subscribe: (run: (value: never) => void) =>
        real.repoStore.subscribe((value) =>
          run({
            ...value,
            currentPath: pill.currentPath,
            terminalOpen: pill.terminalOpen,
          } as never),
        ),
    },
  };
});

const bar = () => render(TerminalPill).body;

function pillTag(body: string): string {
  const tag = /<button\b[^>]*data-testid="terminal-pill"[\s\S]*?>/.exec(body)?.[0];
  expect(tag, body).toBeTruthy();
  return tag ?? "";
}

function handler(name: string, file: string = source): string {
  const start = file.indexOf(`function ${name}(`);
  expect(start).toBeGreaterThan(-1);
  const end = file.indexOf("\n  function ", start + 1);
  return file.slice(start, end === -1 ? file.indexOf("\n  const ", start + 1) : end);
}

beforeEach(() => {
  pill.terminalOpen = false;
  pill.currentPath = "/repo/app";
  interfaceStore.setStatusBarMode("full");
});

afterEach(() => {
  interfaceStore.setStatusBarMode("full");
});

describe("terminal pill", () => {
  it("is no longer part of the status bar", () => {
    expect(statusBarSource).not.toContain("terminal-pill");
    expect(statusBarSource).not.toContain("SquareTerminal");
    expect(statusBarSource).not.toContain("terminalPillView");
    expect(render(StatusBar).body).not.toContain("terminal-pill");
  });

  it("renders nothing without a current repository", () => {
    pill.currentPath = null;
    expect(bar()).not.toContain("terminal-pill");
  });

  it("is a pill, not the old 10px muted text button", () => {
    // The previous control was a borderless `text-[10px]` label. That class
    // string is the regression this test exists to fail on.
    expect(source).not.toContain('class="inline-flex items-center gap-1 transition-colors text-[10px]');
    const body = bar();
    const tag = pillTag(body);
    expect(tag).toContain("rounded-full");
    expect(tag).toContain("h-6");
    expect(tag).toContain("text-meta");
    expect(tag).not.toContain("text-[11px]");
    expect(tag).toContain("text-textPrimary");
    expect(tag).toContain("border-edge");
    expect(tag).not.toContain("text-[10px]");
    expect(tag).toContain('aria-pressed="false"');
    expect(source).toContain("onclick={toggleDock}");
    expect(body).toContain(">Dock<");
    expect(body).not.toContain("No shell");
    expect(body).not.toMatch(/bun|npm test|zsh/);
    expect(handler("toggleDock")).toContain("repoStore.toggleTerminal()");
  });

  it("uses the pressed face and an honest empty summary when the dock is open", () => {
    pill.terminalOpen = true;
    const body = bar();
    const tag = pillTag(body);
    expect(tag).toContain('aria-pressed="true"');
    expect(tag).toContain("bg-accent/10");
    expect(tag).toContain("text-accent");
    expect(body).toContain('data-testid="terminal-pill-summary"');
    expect(body).toContain("No shell");
    expect(body).not.toMatch(/bun|npm|Idle/);
  });

  it("names a published session without inventing a command", () => {
    const before = get(terminalSessions).length;
    const lease = terminalSessions.reserve({
      key: "pill-live",
      repoPath: "/repo/app",
      label: "Shell",
      status: "running",
      close: async () => {},
    });
    try {
      const body = bar();
      expect(body).toContain('data-testid="terminal-pill-count"');
      expect(body).toContain(`>${before + 1}<`);
      if (before === 0) {
        expect(body).toContain('data-testid="terminal-pill-summary"');
        expect(body).toContain(">Shell<");
        expect(body).not.toMatch(/bun|npm|Idle|Running/);
      }
    } finally {
      lease.release();
    }
  });

  it("wires the hover menu to the dock's own actions", () => {
    const menu = menuSource.slice(
      menuSource.indexOf('data-testid="terminal-pill-menu"'),
      menuSource.length,
    );
    expect(menu).toContain("Show dock");
    expect(menu).toContain("Hide dock");
    expect(menu).toContain("Focus session");
    expect(menu).toContain("Switch to ");
    expect(menu).toContain("New {launcherLabel($interfaceStore.terminalLauncher)} session");
    expect(menu).toContain("onToggleDock()");
    expect(menu).toContain("focusListed(");
    expect(menu).toContain("newTerminal()");
    expect(menu).toContain('data-testid="terminal-pill-detail"');
    expect(handler("focusListed", menuSource)).toContain("focusTerminalSession(record)");
    expect(handler("newTerminal", menuSource)).toContain("terminalLaunchRequests.request(");
    expect(handler("newTerminal", menuSource)).toContain("repoStore.setTerminalOpen(true)");
    expect(source).toContain("const MENU_CLOSE_GRACE_MS = 220");
    expect(menuSource).toContain('place: "below" as const');
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain('event.key !== "Escape"');
  });

  it("lazy-loads TerminalPillMenu as its own chunk so the hover menu stays out of the entry bundle", () => {
    expect(source).not.toMatch(/^\s*import\s+TerminalPillMenu\b/m);
    expect(source).toContain('import("./TerminalPillMenu.svelte")');
    expect(source).toContain('diagnostics.error("terminal-pill:menu", error)');
    expect(source).toContain("onToggleDock={toggleDock}");
  });

  it("renders structured session cards for 1 or more sessions with repo logo, repo name, agent icon, and session title", () => {
    const menu = menuSource.slice(
      menuSource.indexOf('data-testid="terminal-pill-menu"'),
      menuSource.length,
    );
    expect(menu).toContain("terminalPill.sessions.length > 0");
    expect(menu).not.toContain("terminalPill.sessions.length > 1");
    expect(menu).toContain('data-testid="terminal-pill-session-title"');
    expect(menu).toContain('data-testid="terminal-pill-session-repo"');
    expect(menu).toContain('data-testid="terminal-pill-session-agent"');
    expect(menu).toContain("<RepoLogo");
    expect(menu).toContain("<LauncherIcon kind={session.launcherKind}");
    expect(menu).toContain("max-w-[9rem] truncate");
    expect(source).toContain("terminalPill.highlightedLauncher");
    expect(source).toContain("liveAct?.title");
  });

  it("embeds hover search to filter agents across repos and quick-launch terminals without pointer-leave dismissal", () => {
    const menu = menuSource.slice(
      menuSource.indexOf('data-testid="terminal-pill-menu"'),
      menuSource.length,
    );
    expect(menu).toContain('data-testid="terminal-pill-search"');
    expect(menu).toContain('role="combobox"');
    expect(menu).toContain('aria-autocomplete="list"');
    expect(menu).toContain('id="terminal-pill-results"');
    expect(menu).toContain('data-testid="terminal-pill-launch"');
    expect(menu).toContain('data-testid="terminal-pill-empty"');
    expect(menu).toContain("newTerminal(launch.repoPath, launch.launcher)");
    expect(menuSource).toContain("searchTerminalPill(");
    expect(menuSource).toContain("pillSearchActions(");
    expect(handler("isMenuPinnedByInteraction", source)).toContain("[data-terminal-pill-menu]");
    expect(handler("isMenuPinnedByInteraction", source)).toContain("focusSearchOnOpen");
    expect(handler("onPointerLeaveMenu", source)).toContain("isMenuPinnedByInteraction()");
    expect(handler("onSearchKeydown", menuSource)).toContain("isImeComposition(event)");
    expect(handler("onSearchKeydown", menuSource)).toContain("stepSearchIndex(");
    expect(handler("onSearchKeydown", menuSource)).toContain("activateSearchAction(");
    expect(handler("onSearchKeydown", menuSource)).toContain("event.shiftKey");
    expect(handler("onMenuKeydown", menuSource)).toContain('event.key === "/"');
    expect(handler("newTerminal", menuSource)).toContain("repoStore.activateTab(");
    expect(handler("newTerminal", menuSource)).toContain("repoStore.trustRepo(");
    expect(handler("newTerminal", menuSource)).toContain("onReady:");
    expect(handler("closeMenu", menuSource)).toContain('searchQuery = ""');
    expect(menu).toContain('data-testid="terminal-pill-filters"');
    expect(menu).toContain('data-testid="terminal-pill-quick-launchers"');
    expect(menu).toContain("highlightSearchText(");
    expect(menuSource).toContain("recentRepos:");
    expect(menuSource).toContain("resolvePillActionIndex(");
    expect(menuSource).toContain("polledSessionKeys");
  });

  it("compiles both pill and hover menu with no Svelte accessibility warnings", () => {
    const pillWarnings = compile(source, { generate: "client", filename: "TerminalPill.svelte" }).warnings;
    const menuWarnings = compile(menuSource, { generate: "client", filename: "TerminalPillMenu.svelte" }).warnings;
    expect(pillWarnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
    expect(menuWarnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });
});
