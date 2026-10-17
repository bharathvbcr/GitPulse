import { describe, expect, it, vi } from "vitest";
import {
  CONTEXT_UNKNOWN, closeQuestion, contextReadOf, isProvablyIdle, parseTerminalContext, readSessionContext, startDirFrom,
  type ContextRead, type SessionVantage, type TerminalContext,
} from "./sessionContext";
import { terminalDeadline } from "./inputQueue";
import { closeTab, initialState, openTab, setTabStartDir } from "./tabs";

const idle: TerminalContext = { process: "zsh", busy: false, jobs: false, cwd: "/repo/crates/core", repo_dir: "crates/core" };
const known = (context: TerminalContext): ContextRead => ({ kind: "known", context });
const NONE: ContextRead = { kind: "none" };

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

describe("a shell with a job that is not in the foreground", () => {
  it("is not idle, and closing it asks", () => {
    const backgrounded = { ...idle, busy: false, jobs: true };
    expect(parseTerminalContext(backgrounded)).toEqual(backgrounded);
    expect(closeQuestion(known(backgrounded), "Shell")?.message).toMatch(/background or stopped job/);
    expect(closeQuestion(known({ ...idle, jobs: false }), "Shell")).toBeNull();
    expect(parseTerminalContext({ ...idle, jobs: "yes" })).toBeNull();
  });
});

describe("where a tab opened beside another starts", () => {
  it("starts in the neighbour's directory when it is inside the repository", () => {
    expect(startDirFrom(known(idle))).toBe("crates/core");
  });

  it("starts at the root when the directory is the root, outside, unknown, or unsafe", () => {
    for (const repo_dir of ["", null, "/etc", "../up", "a/../../b", "..\\win"]) {
      expect(startDirFrom(known({ ...idle, repo_dir })), String(repo_dir)).toBeNull();
    }
    expect(startDirFrom(CONTEXT_UNKNOWN)).toBeNull();
    expect(startDirFrom(NONE)).toBeNull();
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
    expect(closeQuestion(known({ ...idle, busy: true, process: "cargo" }), "Shell")).toEqual({
      title: "Close Shell?",
      message: "cargo is still running in this terminal. Closing it stops it.",
    });
    expect(closeQuestion(known({ ...idle, busy: true, process: null }), "Shell")?.message)
      .toBe("A program is still running in this terminal. Closing it stops that program.");
    expect(closeQuestion(known(idle), "Shell")).toBeNull();
    expect(closeQuestion(NONE, "Shell")).toBeNull();
  });

  // The defect: an agent tab's PTY child *is* the agent, so the foreground
  // leader is the root and `busy` reads false. Closing a working Claude Code
  // tab — or a task attempt's — killed it without a word.
  it("asks before stopping a running agent even though the OS reports it idle", () => {
    const claude = { name: "Claude", running: true, taskAttempt: false };
    const leaderIsTheAgent: TerminalContext = { process: "claude", busy: false, jobs: false, cwd: "/repo", repo_dir: "" };
    for (const context of [known(leaderIsTheAgent), CONTEXT_UNKNOWN, NONE, known({ ...leaderIsTheAgent, busy: null })]) {
      expect(closeQuestion(context, "Claude", claude)).toEqual({
        title: "Close Claude?",
        message: "Claude is still running in this tab. Closing it stops the agent.",
      });
    }
    const attempt = closeQuestion(known(leaderIsTheAgent), "Fix the importer", { ...claude, taskAttempt: true });
    expect(attempt?.title).toBe("Close Fix the importer?");
    expect(attempt?.message).toContain("ends this attempt");
    expect(attempt?.message).toContain("resumed");
  });

  it("closes an agent that has already ended without asking, and still asks for a busy shell job", () => {
    const ended = { name: "Claude", running: false, taskAttempt: true };
    expect(closeQuestion(known({ process: "claude", busy: false, jobs: false, cwd: "/repo", repo_dir: "" }), "Claude", ended)).toBeNull();
    expect(closeQuestion(known({ ...idle, busy: true, process: "cargo" }), "Claude", ended)?.message).toContain("cargo is still running");
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

// The defect: a context read that timed out, failed or came back unreadable
// returned null, and null closed the tab without a word — so a shell with a
// running job was killed because the host happened to be slow for five
// seconds. "Could not tell" must never read as "nothing is running".
describe("closing when it cannot be told whether anything is running", () => {
  const COULD_NOT_TELL = {
    title: "Close Shell?",
    message: "GitPulse could not tell whether something is still running in this terminal. Close it anyway?",
  };

  it("asks, and says it could not tell, when the read produced no answer", () => {
    expect(closeQuestion(CONTEXT_UNKNOWN, "Shell")).toEqual(COULD_NOT_TELL);
  });

  it("asks when the host answered but the foreground or the job list was unreadable", () => {
    for (const patch of [{ busy: null }, { jobs: null }, { jobs: undefined }, { busy: null, jobs: null }]) {
      const context = { ...idle, ...patch } as TerminalContext;
      expect(closeQuestion(known(context), "Shell"), JSON.stringify(patch)).toEqual(COULD_NOT_TELL);
    }
    // The host's all-null answer, which is also what it sends for a child that is already gone.
    expect(closeQuestion(known({ process: null, busy: null, cwd: null, repo_dir: null }), "Shell")).toEqual(COULD_NOT_TELL);
  });

  it("still names a job it does know about when another field is unreadable", () => {
    expect(closeQuestion(known({ ...idle, busy: true, jobs: null, process: "cargo" }), "Shell")?.message).toContain("cargo is still running");
    expect(closeQuestion(known({ ...idle, busy: null, jobs: true }), "Shell")?.message).toMatch(/background or stopped job/);
  });

  it("asks about an agent tab that has ended but whose shell cannot be read", () => {
    const ended = { name: "Claude", running: false, taskAttempt: false };
    expect(closeQuestion(CONTEXT_UNKNOWN, "Shell", ended)).toEqual(COULD_NOT_TELL);
    expect(closeQuestion(NONE, "Shell", ended)).toBeNull();
  });

  it("calls a shell idle only when both the foreground and the jobs are positively false", () => {
    expect(isProvablyIdle(idle)).toBe(true);
    expect(isProvablyIdle({ ...idle, busy: null })).toBe(false);
    expect(isProvablyIdle({ ...idle, jobs: null })).toBe(false);
    expect(isProvablyIdle({ process: "zsh", busy: false, cwd: null, repo_dir: null })).toBe(false);
  });
});

describe("reading a session's context", () => {
  const live: SessionVantage = { sessionId: "term-1", current: true, spawning: false, exited: false };
  const answers = (raw: unknown) => async () => raw;

  it("is known when the host answers with a well-formed context", async () => {
    expect(await readSessionContext(live, answers(idle))).toEqual(known(idle));
  });

  it("is unknown when the host answers with something malformed", async () => {
    expect(contextReadOf({ ...idle, busy: "yes" })).toEqual(CONTEXT_UNKNOWN);
    expect(await readSessionContext(live, answers("zsh"))).toEqual(CONTEXT_UNKNOWN);
    expect(await readSessionContext(live, answers(null))).toEqual(CONTEXT_UNKNOWN);
  });

  it("is unknown when the read fails", async () => {
    expect(await readSessionContext(live, async () => { throw new Error("Terminal session 'term-1' not found"); })).toEqual(CONTEXT_UNKNOWN);
  });

  it("is unknown when the read never answers within its deadline", async () => {
    vi.useFakeTimers();
    try {
      const never = new Promise<unknown>(() => {});
      const pending = readSessionContext(live, () => terminalDeadline(never, 5_000, "Reading terminal context"));
      await vi.advanceTimersByTimeAsync(5_500);
      expect(await pending).toEqual(CONTEXT_UNKNOWN);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not ask the host when the tab cannot vouch for the session, and does not say nothing is running", async () => {
    const read = vi.fn(answers(idle));
    expect(await readSessionContext({ ...live, current: false }, read)).toEqual(CONTEXT_UNKNOWN);
    expect(await readSessionContext({ ...live, sessionId: "", spawning: true }, read)).toEqual(CONTEXT_UNKNOWN);
    expect(read).not.toHaveBeenCalled();
  });

  it("is none only on the tab's own evidence that no process exists", async () => {
    const read = vi.fn(answers(idle));
    expect(await readSessionContext({ ...live, exited: true }, read)).toEqual(NONE);
    expect(await readSessionContext({ ...live, sessionId: "" }, read)).toEqual(NONE);
    expect(read).not.toHaveBeenCalled();
  });
});
