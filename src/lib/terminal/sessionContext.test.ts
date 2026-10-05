import { describe, expect, it } from "vitest";
import { closeQuestion, parseTerminalContext, startDirFrom, type TerminalContext } from "./sessionContext";
import { closeTab, initialState, openTab, setTabStartDir } from "./tabs";

const idle: TerminalContext = { process: "zsh", busy: false, cwd: "/repo/crates/core", repo_dir: "crates/core" };

describe("terminal context at the boundary", () => {
  it("accepts the backend's shape, unknowns included", () => {
    expect(parseTerminalContext(idle)).toEqual(idle);
    expect(parseTerminalContext({ process: null, busy: null, cwd: null, repo_dir: null }))
      .toEqual({ process: null, busy: null, cwd: null, repo_dir: null });
    expect(parseTerminalContext({})).toEqual({ process: null, busy: null, cwd: null, repo_dir: null });
  });

  it("refuses anything malformed rather than guessing", () => {
    for (const bad of [
      null, undefined, 7, "zsh", [],
      { ...idle, busy: "yes" },
      { ...idle, busy: 1 },
      { ...idle, process: 42 },
      { ...idle, cwd: {} },
      { ...idle, repo_dir: "a\0b" },
      { ...idle, cwd: "x".repeat(5000) },
    ]) {
      expect(parseTerminalContext(bad), JSON.stringify(bad)?.slice(0, 40)).toBeNull();
    }
  });
});

describe("where a tab opened beside another starts", () => {
  it("starts in the neighbour's directory when it is inside the repository", () => {
    expect(startDirFrom(idle)).toBe("crates/core");
  });

  it("starts at the root when the directory is the root, outside, unknown, or unsafe", () => {
    for (const repo_dir of ["", null, "/etc", "../up", "a/../../b", "..\\win"]) {
      expect(startDirFrom({ ...idle, repo_dir }), String(repo_dir)).toBeNull();
    }
    expect(startDirFrom(null)).toBeNull();
  });

  it("pins the directory on the new tab only", () => {
    const opened = openTab(initialState(), "shell");
    const id = opened.activeId!;
    const pinned = setTabStartDir(opened, id, "crates/core");
    expect(pinned.tabs.find((tab) => tab.id === id)?.startDir).toBe("crates/core");
    expect(pinned.tabs.filter((tab) => tab.startDir).length).toBe(1);
    expect(setTabStartDir(opened, id, null)).toBe(opened);
    expect(setTabStartDir(opened, "tab-missing", "x")).toBe(opened);
  });
});

describe("closing a terminal with a job in it", () => {
  it("asks only when the foreground is known to be a running job", () => {
    expect(closeQuestion({ ...idle, busy: true, process: "cargo" }, "Shell")).toEqual({
      title: "Close Shell?",
      message: "cargo is still running in this terminal. Closing it stops it.",
    });
    expect(closeQuestion({ ...idle, busy: true, process: null }, "Shell")?.message)
      .toBe("A program is still running in this terminal. Closing it stops that program.");
    expect(closeQuestion(idle, "Shell")).toBeNull();
    expect(closeQuestion({ ...idle, busy: null }, "Shell")).toBeNull();
    expect(closeQuestion(null, "Shell")).toBeNull();
  });

  // The defect: an agent tab's PTY child *is* the agent, so the foreground
  // leader is the root and `busy` reads false. Closing a working Claude Code
  // tab — or a task attempt's — killed it without a word.
  it("asks before stopping a running agent even though the OS reports it idle", () => {
    const claude = { name: "Claude", running: true, taskAttempt: false };
    const leaderIsTheAgent: TerminalContext = { process: "claude", busy: false, cwd: "/repo", repo_dir: "" };
    for (const context of [leaderIsTheAgent, null, { ...leaderIsTheAgent, busy: null }]) {
      expect(closeQuestion(context, "Claude", claude)).toEqual({
        title: "Close Claude?",
        message: "Claude is still running in this tab. Closing it stops the agent.",
      });
    }
    const attempt = closeQuestion(leaderIsTheAgent, "Fix the importer", { ...claude, taskAttempt: true });
    expect(attempt?.title).toBe("Close Fix the importer?");
    expect(attempt?.message).toContain("ends this attempt");
    expect(attempt?.message).toContain("resumed");
  });

  it("closes an agent that has already ended without asking, and still asks for a busy shell job", () => {
    const ended = { name: "Claude", running: false, taskAttempt: true };
    expect(closeQuestion({ process: "claude", busy: false, cwd: "/repo", repo_dir: "" }, "Claude", ended)).toBeNull();
    expect(closeQuestion({ ...idle, busy: true, process: "cargo" }, "Claude", ended)?.message).toContain("cargo is still running");
  });
});

describe("closing one half of a split", () => {
  it("shows the other half, not a neighbour", () => {
    let state = initialState();
    state = openTab(state, "shell");
    state = openTab(state, "shell");
    const [a, b, c] = state.tabs.map((tab) => tab.id);
    // Split shows a and c; b sits between them in the strip.
    state = { ...state, activeId: a };
    expect(closeTab(state, a).activeId).toBe(b);
    expect(closeTab(state, a, c).activeId).toBe(c);
    expect(closeTab(state, a, "tab-gone").activeId).toBe(b);
    expect(closeTab(state, a, a).activeId).toBe(b);
  });
});
