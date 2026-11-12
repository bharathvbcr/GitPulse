import { describe, expect, it } from "vitest";
import type { TerminalContext } from "./sessionContext";
import { activityPhrase, terminalPillView, type PillSession } from "./statusPill";

const identity = { caseInsensitive: false };

function shell(overrides: Partial<PillSession> = {}): PillSession {
  return {
    key: "a",
    repoPath: "/repo/app",
    label: "Shell",
    status: "running",
    canReveal: true,
    ...overrides,
  };
}

const idle: TerminalContext = {
  process: "zsh",
  busy: false,
  jobs: false,
  cwd: "/repo/app/src",
  repo_dir: "src",
};

const busy: TerminalContext = {
  process: "bun",
  busy: true,
  jobs: true,
  cwd: "/repo/app",
  repo_dir: "",
};

describe("terminal pill copy", () => {
  it("says nothing about a command when the dock is closed and no session is published", () => {
    const view = terminalPillView([], { open: false, repoPath: "/repo/app", identity });
    expect(view.summary).toBeNull();
    expect(view.detail).toBe("No shell · dock closed");
    expect(view.alive).toBe(0);
    expect(view.focusKey).toBeNull();
  });

  it("says No shell when the dock is open and the host has published nothing", () => {
    const view = terminalPillView([], { open: true, repoPath: "/repo/app", identity });
    expect(view.summary).toBe("No shell");
    expect(view.detail).toBe("No shell");
    expect(view.summary).not.toMatch(/bun|npm|zsh|Idle|Running/);
  });

  it("does not treat a live PTY as a running command or as idle", () => {
    expect(activityPhrase("running", null)).toBeNull();
    const view = terminalPillView([shell()], { open: false, repoPath: "/repo/app", identity });
    expect(view.summary).toBe("Shell");
    expect(view.detail).toBe("Shell");
    expect(view.summary).not.toMatch(/bun|npm|Idle|Running/);
    expect(view.sessions[0]?.cwd).toBeNull();
  });

  it("says Idle only when the host proved the shell idle, and names a program only when the host said it is busy", () => {
    expect(activityPhrase("running", idle)).toBe("Idle");
    expect(activityPhrase("running", busy)).toBe("bun");
    expect(activityPhrase("running", { ...busy, process: null })).toBe("Busy");
    expect(activityPhrase("running", { ...busy, process: "   " })).toBe("Busy");
    const idling = terminalPillView([shell()], {
      open: true,
      repoPath: "/repo/app",
      identity,
      activity: { a: idle },
    });
    expect(idling.summary).toBe("Shell · Idle");
    expect(idling.sessions[0]?.cwd).toBe("/repo/app/src");
    expect(idling.sessions[0]?.where).toBe("src");
    const running = terminalPillView([shell()], {
      open: true,
      repoPath: "/repo/app",
      identity,
      activity: { a: busy },
    });
    expect(running.summary).toBe("Shell · bun");
  });

  it("prefers the session title and a non-running lifecycle status the registry published", () => {
    const view = terminalPillView(
      [shell({ title: "Fix importer", status: "starting", checkout: "/repo/app/.claude/worktrees/fix" })],
      { open: true, repoPath: "/repo/app", identity },
    );
    expect(view.summary).toBe("Fix importer · Starting");
    expect(view.sessions[0]?.cwd).toBe("/repo/app/.claude/worktrees/fix");
    expect(view.sessions[0]?.where).toBe("fix");
    expect(view.focusKey).toBe("a");
  });

  it("lists every session when several are alive, current repository first, and focuses only a single revealable one", () => {
    const here = shell({ key: "here", label: "Shell" });
    const elsewhere = shell({ key: "else", repoPath: "/repo/other", label: "Claude", title: "Review" });
    const hidden = shell({ key: "hidden", canReveal: false });
    const many = terminalPillView([elsewhere, here], { open: true, repoPath: "/repo/app", identity });
    expect(many.alive).toBe(2);
    expect(many.summary).toBeNull();
    expect(many.detail).toBe("2 sessions");
    expect(many.focusKey).toBeNull();
    expect(many.sessions.map((session) => session.key)).toEqual(["here", "else"]);
    expect(many.sessions[1]?.name).toBe("Review");
    const one = terminalPillView([hidden], { open: true, repoPath: "/repo/app", identity });
    expect(one.focusKey).toBeNull();
    expect(one.alive).toBe(1);
  });
});
