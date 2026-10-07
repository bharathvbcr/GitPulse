import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";

const openRepo = vi.fn();
const setTerminalOpen = vi.fn();
const setGlobalSurface = vi.fn();
const repoState = writable<{ currentPath: string | null }>({ currentPath: "/work/current" });
vi.mock("../stores/repoStore", () => ({ repoStore: { subscribe: repoState.subscribe, openRepo: (...args: unknown[]) => openRepo(...args), setTerminalOpen: (...args: unknown[]) => setTerminalOpen(...args) } }));
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: (...args: unknown[]) => setGlobalSurface(...args) } }));
const findConversation = vi.fn();
vi.mock("./client", () => ({ findConversation: (...args: unknown[]) => findConversation(...args) }));
const resolveGitRoot = vi.fn();
vi.mock("../desktop/nativeShell", () => ({ resolveGitRoot: (...args: unknown[]) => resolveGitRoot(...args) }));
const focusTerminalSession = vi.fn();
vi.mock("../terminal/sessionFocus", () => ({ focusTerminalSession: (...args: unknown[]) => focusTerminalSession(...args) }));

const { openAttemptCheckout, queuedTerminalNote, resumeTaskConversation, showTaskTerminal, startTaskTerminal } = await import("./taskTerminal");
const { consumeTaskTerminalRequest, taskTerminalRequests } = await import("../terminal/taskLaunches");
const { terminalSessions } = await import("../terminal/sessionRegistry");

const run = (id: string, cwd: string) => ({ id, cwd, provider: "claude" as const, task_title: `Task ${id}` });
const SESSION = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

const slots: { release(): void }[] = [];
function liveSession(fields: { key: string; taskRunId?: string; continuesRunId?: string; reveal?: boolean }) {
  slots.push(terminalSessions.reserve({
    key: fields.key, repoPath: "/work/a", label: "Claude Code", status: "running", close: async () => {},
    ...(fields.taskRunId ? { taskRunId: fields.taskRunId } : {}),
    ...(fields.continuesRunId ? { continuesRunId: fields.continuesRunId } : {}),
    ...(fields.reveal === false ? {} : { reveal: () => {} }),
  }));
}

beforeEach(() => {
  repoState.set({ currentPath: "/work/current" });
  openRepo.mockReset(); setTerminalOpen.mockReset(); setGlobalSurface.mockReset(); findConversation.mockReset(); focusTerminalSession.mockReset();
  // A checkout root resolves to itself.
  resolveGitRoot.mockReset(); resolveGitRoot.mockImplementation(async (path: string) => path);
});
afterEach(() => {
  for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request);
  for (const slot of slots.splice(0)) slot.release();
});

describe("startTaskTerminal", () => {
  it("queues the request before the checkout opens, and opens it in the background", async () => {
    openRepo.mockImplementation(async (_path: string, options: { activate?: boolean }) => {
      // The request is already waiting when the dock could first host it.
      expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["a"]);
      expect(options).toEqual({ activate: false });
      return true;
    });
    expect(await startTaskTerminal(run("a", "/work/a"))).toBe("started");
  });

  it("never takes the reader off the task: no surface change, no dock opened", async () => {
    // The defect this module was rewritten for. A launch from a task sheet
    // used to switch to the repository surface and open its dock, which
    // took the reader off the sheet that launched it.
    openRepo.mockResolvedValue(true);
    await startTaskTerminal(run("a", "/work/a"));
    expect(setGlobalSurface).not.toHaveBeenCalled();
    expect(setTerminalOpen).not.toHaveBeenCalled();
    expect(openRepo).toHaveBeenCalledTimes(1);
    expect(openRepo.mock.calls[0][1]).not.toHaveProperty("onReady");
  });

  it("makes the checkout the active repository when none is, still without a surface change", async () => {
    // The dock only exists inside the repository view, which renders only
    // while some repository is current. A background tab with nothing active
    // would wait for a dock that never mounts: the agent would never start.
    repoState.set({ currentPath: null });
    openRepo.mockResolvedValue(true);
    expect(await startTaskTerminal(run("a", "/work/a"))).toBe("started");
    expect(openRepo).toHaveBeenCalledWith("/work/a", { activate: true });
    expect(setGlobalSurface).not.toHaveBeenCalled();
    expect(setTerminalOpen).not.toHaveBeenCalled();
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/work/a", mode: "ask", reason: "saved" });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(openRepo).toHaveBeenLastCalledWith("/work/a", { activate: true });
  });

  it("keeps the terminal queued when the checkout cannot be opened", async () => {
    openRepo.mockResolvedValueOnce(false);
    expect(await startTaskTerminal(run("c", "/work/c"))).toBe("queued");
    expect(get(taskTerminalRequests)).toEqual([{ runId: "c", repoPath: "/work/c", provider: "claude", title: "Task c" }]);
  });

  it("keeps each attempt's request when launches overlap", async () => {
    let release: (value: boolean) => void = () => {};
    openRepo.mockImplementationOnce(() => new Promise<boolean>((resolve) => { release = resolve; }));
    openRepo.mockResolvedValueOnce(true);
    const first = startTaskTerminal(run("a", "/work/a"));
    const second = startTaskTerminal(run("b", "/work/b"));
    // Both opens are in flight (each first resolves its checkout root).
    await vi.waitFor(() => expect(openRepo).toHaveBeenCalledTimes(2));
    release(false);
    expect(await first).toBe("queued");
    expect(await second).toBe("started");
    expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["a", "b"]);
  });

  it("propagates a failed open rather than reporting a start", async () => {
    openRepo.mockRejectedValueOnce(new Error("trust prompt failed"));
    await expect(startTaskTerminal(run("d", "/work/d"))).rejects.toThrow("trust prompt failed");
    // The request stands: the checkout opening later still starts it.
    expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["d"]);
  });
});

describe("an attempt whose directory is below its checkout's root", () => {
  // `openRepo` opens checkouts; the host refuses a directory with no `.git`
  // of its own. Opening the attempt's subdirectory failed every time, so the
  // launch reported "queued" and waited for a tab that could never open.
  const SUB = "/work/a/packages/web";

  it("opens the checkout that contains it, and queues the terminal for that checkout", async () => {
    resolveGitRoot.mockImplementation(async (path: string) => (path === SUB ? "/work/a" : path));
    openRepo.mockImplementation(async (path: string) => {
      expect(get(taskTerminalRequests)).toEqual([{ runId: "a", repoPath: "/work/a", provider: "claude", title: "Task a", startDir: "packages/web" }]);
      return path === "/work/a";
    });
    expect(await startTaskTerminal(run("a", SUB))).toBe("started");
    expect(openRepo).toHaveBeenCalledWith("/work/a", { activate: false });
  });

  it("shows it the same way", async () => {
    resolveGitRoot.mockImplementation(async (path: string) => (path === SUB ? "/work/a" : path));
    openRepo.mockImplementation(async (path: string, options: { onReady: () => void }) => { if (path === "/work/a") options.onReady(); return path === "/work/a"; });
    expect(await showTaskTerminal(run("a", SUB))).toBe("opened");
  });

  it("resumes a conversation in the subdirectory it ran in, inside the checkout's tab", async () => {
    resolveGitRoot.mockImplementation(async (path: string) => (path === SUB ? "/work/a" : path));
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: SUB, mode: "ask", reason: "saved" });
    openRepo.mockImplementation(async (path: string) => path === "/work/a");
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(get(taskTerminalRequests)[0]).toMatchObject({ repoPath: "/work/a", startDir: "packages/web" });
  });

  it("says so, and queues nothing, when no checkout contains the directory", async () => {
    resolveGitRoot.mockRejectedValue(new Error("Not a Git repository: /tmp/loose"));
    await expect(startTaskTerminal(run("z", "/tmp/loose"))).rejects.toThrow(/No Git checkout contains \/tmp\/loose/);
    await expect(showTaskTerminal(run("z", "/tmp/loose"))).rejects.toThrow(/No Git checkout contains/);
    expect(openRepo).not.toHaveBeenCalled();
    // Never "queued": nothing would ever open it.
    expect(get(taskTerminalRequests)).toEqual([]);
  });
});

describe("openAttemptCheckout", () => {
  it("opens the attempt's checkout as the active tab, in front, without touching its terminal", async () => {
    resolveGitRoot.mockImplementation(async (path: string) => (path === "/work/a/pkg" ? "/work/a" : path));
    openRepo.mockImplementation(async (_path: string, options: { onReady: () => void }) => { options.onReady(); return true; });
    expect(await openAttemptCheckout({ cwd: "/work/a/pkg" })).toBe(true);
    expect(openRepo).toHaveBeenCalledWith("/work/a", expect.objectContaining({ activate: true }));
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
    // Opening a checkout is not starting or showing an agent.
    expect(get(taskTerminalRequests)).toEqual([]);
    expect(setTerminalOpen).not.toHaveBeenCalled();
  });

  it("stays where it is when the checkout cannot be opened, and says why when none holds it", async () => {
    openRepo.mockResolvedValueOnce(false);
    expect(await openAttemptCheckout({ cwd: "/work/a" })).toBe(false);
    expect(setGlobalSurface).not.toHaveBeenCalled();
    resolveGitRoot.mockRejectedValueOnce(new Error("Not a Git repository: /gone"));
    await expect(openAttemptCheckout({ cwd: "/gone" })).rejects.toThrow(/No Git checkout contains \/gone/);
  });
});

describe("showTaskTerminal", () => {
  it("focuses the attempt's running session through the focus owner", async () => {
    liveSession({ key: "tab-1", taskRunId: "a" });
    focusTerminalSession.mockResolvedValueOnce({ ok: true, switchedRepo: true, openedDock: true });
    expect(await showTaskTerminal(run("a", "/work/a"))).toBe("opened");
    expect(focusTerminalSession.mock.calls[0][0]).toMatchObject({ key: "tab-1", taskRunId: "a" });
    expect(openRepo).not.toHaveBeenCalled();
    expect(get(taskTerminalRequests)).toEqual([]);
  });

  it("opens the checkout in front when the attempt has no session yet", async () => {
    openRepo.mockImplementation(async (_path: string, options: { onReady: (path: string) => void }) => {
      expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["a"]);
      options.onReady("/work/a");
      return true;
    });
    expect(await showTaskTerminal(run("a", "/work/a"))).toBe("opened");
    expect(focusTerminalSession).not.toHaveBeenCalled();
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
    expect(setTerminalOpen).toHaveBeenCalledWith(true);
  });

  it("falls back to the checkout when the session cannot be focused", async () => {
    liveSession({ key: "tab-1", taskRunId: "a" });
    focusTerminalSession.mockResolvedValueOnce({ ok: false, reason: "unavailable" });
    openRepo.mockImplementation(async (_path: string, options: { onReady: () => void }) => { options.onReady(); return true; });
    expect(await showTaskTerminal(run("a", "/work/a"))).toBe("opened");
    expect(openRepo).toHaveBeenCalledTimes(1);
  });

  it("never treats a session adopted after a reload as the attempt's own tab", async () => {
    // Its reveal hands the process to a new tab; focusing it would show nothing.
    liveSession({ key: "detached:term-1", taskRunId: "a" });
    openRepo.mockResolvedValueOnce(true);
    expect(await showTaskTerminal(run("a", "/work/a"))).toBe("queued");
    expect(focusTerminalSession).not.toHaveBeenCalled();
  });

  it("treats an open that reports success without becoming ready as queued", async () => {
    openRepo.mockResolvedValueOnce(true);
    expect(await showTaskTerminal(run("c", "/work/c"))).toBe("queued");
    expect(setTerminalOpen).not.toHaveBeenCalled();
  });

  it("says where the waiting terminal will start", () => {
    expect(queuedTerminalNote("/work/repo/.gitpulse/worktrees/fix-a1b2c3d4")).toContain("waiting for fix-a1b2c3d4 to open");
    expect(queuedTerminalNote("C:\\work\\repo\\")).toContain("waiting for repo to open");
  });
});

describe("resumeTaskConversation", () => {
  it("resumes the saved conversation in the background by default, in the attempt's mode", async () => {
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/work/a/.gitpulse/worktrees/fix-1", mode: "inspect", reason: "saved" });
    openRepo.mockResolvedValueOnce(true);
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(openRepo).toHaveBeenCalledWith("/work/a/.gitpulse/worktrees/fix-1", { activate: false });
    expect(setGlobalSurface).not.toHaveBeenCalled();
    expect(get(taskTerminalRequests)).toEqual([
      { runId: "a", repoPath: "/work/a/.gitpulse/worktrees/fix-1", provider: "claude", title: "Fix", resume: { sessionId: SESSION, mode: "inspect", runId: "a" } },
    ]);
  });

  it("shows the resumed conversation when asked to, focusing one already open", async () => {
    findConversation.mockResolvedValue({ resumable: true, sessionId: SESSION, cwd: "/work/a", mode: "ask", reason: "saved" });
    openRepo.mockImplementation(async (_path: string, options: { onReady: () => void }) => { options.onReady(); return true; });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" }, "show")).toEqual({ outcome: "opened" });
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
    liveSession({ key: "tab-2", continuesRunId: "a" });
    focusTerminalSession.mockResolvedValueOnce({ ok: true, switchedRepo: false, openedDock: false });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" }, "show")).toEqual({ outcome: "opened" });
    expect(focusTerminalSession.mock.calls[0][0]).toMatchObject({ key: "tab-2" });
    expect(openRepo).toHaveBeenCalledTimes(1);
  });

  it("opens nothing when there is nothing to resume, and says why", async () => {
    findConversation.mockResolvedValueOnce({ resumable: false, reason: "Claude Code has no saved conversation for this attempt." });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "unavailable", reason: "Claude Code has no saved conversation for this attempt." });
    expect(openRepo).not.toHaveBeenCalled();
    expect(get(taskTerminalRequests)).toEqual([]);
  });
});
