/**
 * The status-bar terminal control is a pill, not the old 10px muted label.
 * Server render covers the closed and open faces. The hover menu is closed
 * until pointer or focus, so its actions are pinned from the component source
 * the same way the rest of StatusBar's wiring is.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { render } from "svelte/server";
import StatusBar from "./StatusBar.svelte";
import { interfaceStore } from "../stores/interfaceStore";
import { terminalSessions } from "../terminal/sessionRegistry";

const source = readFileSync(
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

const bar = () => render(StatusBar).body;

function pillTag(body: string): string {
  const tag = /<button\b[^>]*data-testid="terminal-pill"[\s\S]*?>/.exec(body)?.[0];
  expect(tag, body).toBeTruthy();
  return tag ?? "";
}

function handler(name: string): string {
  const start = source.indexOf(`function ${name}(`);
  expect(start).toBeGreaterThan(-1);
  const end = source.indexOf("\n  function ", start + 1);
  return source.slice(start, end === -1 ? source.indexOf("\n  const ", start + 1) : end);
}

beforeEach(() => {
  pill.terminalOpen = false;
  pill.currentPath = "/repo/app";
  interfaceStore.setStatusBarMode("full");
});

afterEach(() => {
  interfaceStore.setStatusBarMode("full");
});

describe("StatusBar terminal pill", () => {
  it("is a pill, not the old 10px muted text button", () => {
    // The previous control was a borderless `text-[10px]` label. That class
    // string is the regression this test exists to fail on.
    expect(source).not.toContain('class="inline-flex items-center gap-1 transition-colors text-[10px]');
    const body = bar();
    const tag = pillTag(body);
    expect(tag).toContain("rounded-full");
    expect(tag).toContain("h-5");
    expect(tag).toContain("text-[11px]");
    expect(tag).toContain("text-textPrimary");
    expect(tag).toContain("border-border");
    expect(tag).not.toContain("text-[10px]");
    expect(tag).toContain('aria-pressed="false"');
    expect(source).toContain("onclick={toggleDock}");
    expect(body).toContain(">Terminal<");
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
    const menu = source.slice(
      source.indexOf('data-testid="terminal-pill-menu"'),
      source.indexOf("Open Command Palette"),
    );
    expect(menu).toContain("Show terminal");
    expect(menu).toContain("Hide terminal");
    expect(menu).toContain("Focus session");
    expect(menu).toContain("Switch to ");
    expect(menu).toContain("New {launcherLabel($interfaceStore.terminalLauncher)} session");
    expect(menu).toContain("toggleDock()");
    expect(menu).toContain("focusListed(");
    expect(menu).toContain("newTerminal()");
    expect(menu).toContain('data-testid="terminal-pill-detail"');
    expect(handler("focusListed")).toContain("focusTerminalSession(record)");
    expect(handler("newTerminal")).toContain("terminalLaunchRequests.request(");
    expect(handler("newTerminal")).toContain("repoStore.setTerminalOpen(true)");
    expect(source).toContain("const MENU_CLOSE_GRACE_MS = 220");
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain('event.key !== "Escape"');
  });
});
