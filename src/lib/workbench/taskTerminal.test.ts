import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get, writable } from "svelte/store";

const openRepo = vi.fn();
const setTerminalOpen = vi.fn();
const setGlobalSurface = vi.fn();
const repoState = writable<{ currentPath: string | null; openTabs?: { id: string; path: string; trustRequired?: boolean; missing?: boolean; family?: string | null; familyRoot?: string | null }[] }>({ currentPath: "/work/current" });
// The repository family of a checkout, as repoStore.familyOf answers it. None by default: a checkout stands alone.
const familyOf = vi.fn();
const activateTab = vi.fn();
const trustTab = vi.fn();
// The store's capacity answer, as repoStore.openRefusal gives it for the published tabs.
const openRefusal = (path: string): string | null => {
  const tabs = (get(repoState) as { openTabs?: { path: string }[] }).openTabs ?? [];
  return tabs.length >= 24 && !tabs.some((tab) => tab.path === path)
    ? "Too many open repositories (max 24). Close a tab to open another."
    : null;
};
vi.mock("../stores/repoStore", () => ({ repoStore: { subscribe: repoState.subscribe, openRepo: (...args: unknown[]) => openRepo(...args), setTerminalOpen: (...args: unknown[]) => setTerminalOpen(...args), activateTab: (...args: unknown[]) => activateTab(...args), trustTab: (...args: unknown[]) => trustTab(...args), openRefusal: (path: string) => openRefusal(path), familyOf: (...args: unknown[]) => familyOf(...args) } }));
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: (...args: unknown[]) => setGlobalSurface(...args) } }));
const findConversation = vi.fn();
vi.mock("./client", async (importOriginal) => ({ ...(await importOriginal<typeof import("./client")>()), findConversation: (...args: unknown[]) => findConversation(...args), explainError: (cause: unknown) => (cause instanceof Error ? cause.message : String(cause)), launchManagedRun: vi.fn() }));
const resolveGitRoot = vi.fn();
vi.mock("../desktop/nativeShell", () => ({ resolveGitRoot: (...args: unknown[]) => resolveGitRoot(...args) }));
const focusTerminalSession = vi.fn();
vi.mock("../terminal/sessionFocus", () => ({ focusTerminalSession: (...args: unknown[]) => focusTerminalSession(...args) }));

const { openAttemptCheckout, queuedTerminalNote, resumeTaskConversation, showAttemptTerminal, showTaskTerminal, startTaskTerminal, trustAttemptCheckout } = await import("./taskTerminal");
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
  openRepo.mockReset(); activateTab.mockReset(); trustTab.mockReset(); setTerminalOpen.mockReset(); setGlobalSurface.mockReset(); findConversation.mockReset(); focusTerminalSession.mockReset();
  // A checkout root resolves to itself.
  resolveGitRoot.mockReset(); resolveGitRoot.mockImplementation(async (path: string) => path);
  familyOf.mockReset(); familyOf.mockResolvedValue(null);
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
      expect(options).toEqual({ activate: false, deferTrust: true });
      return true;
    });
    expect(await startTaskTerminal(run("a", "/work/a"))).toEqual({ kind: "started" });
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
    expect(await startTaskTerminal(run("a", "/work/a"))).toEqual({ kind: "started" });
    expect(openRepo).toHaveBeenCalledWith("/work/a", { activate: true, deferTrust: true });
    expect(setGlobalSurface).not.toHaveBeenCalled();
    expect(setTerminalOpen).not.toHaveBeenCalled();
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/work/a", mode: "ask", reason: "saved" });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(openRepo).toHaveBeenLastCalledWith("/work/a", { activate: true, deferTrust: true });
  });

  it("keeps the terminal waiting, and says for which checkout, when an open is answered without a reason", async () => {
    openRepo.mockResolvedValueOnce(false);
    expect(await startTaskTerminal(run("c", "/work/c"))).toEqual({ kind: "waiting", reason: "checkout", checkout: "/work/c" });
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
    expect(await first).toEqual({ kind: "waiting", reason: "checkout", checkout: "/work/a" });
    expect(await second).toEqual({ kind: "started" });
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
    expect(await startTaskTerminal(run("a", SUB))).toEqual({ kind: "started" });
    expect(openRepo).toHaveBeenCalledWith("/work/a", { activate: false, deferTrust: true });
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

  it("resumes at the root when the host's canonical root spells the recorded checkout through a link", async () => {
    // find_git_root canonicalizes (/tmp -> /private/tmp). The recorded
    // directory naming the same checkout through the link is the root itself,
    // not a reason to refuse the resume.
    resolveGitRoot.mockResolvedValue("/private/tmp/repo");
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/tmp/repo", mode: "ask", reason: "saved" });
    openRepo.mockResolvedValue(true);
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(get(taskTerminalRequests)[0]).toMatchObject({ repoPath: "/private/tmp/repo" });
    expect(get(taskTerminalRequests)[0]).not.toHaveProperty("startDir");
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
    expect(queuedTerminalNote("/work/repo/.gitpulse/worktrees/fix-a1b2c3d4")).toMatch(/^Waiting for fix-a1b2c3d4 to open\./);
    expect(queuedTerminalNote("C:\\work\\repo\\")).toMatch(/^Waiting for repo to open\./);
  });
});

describe("resumeTaskConversation", () => {
  it("resumes the saved conversation in the background by default, in the attempt's mode", async () => {
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/work/a/.gitpulse/worktrees/fix-1", mode: "inspect", reason: "saved" });
    openRepo.mockResolvedValueOnce(true);
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(openRepo).toHaveBeenCalledWith("/work/a/.gitpulse/worktrees/fix-1", { activate: false, deferTrust: true });
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

describe("an open the repository store will refuse", () => {
  const full = (paths: string[]) => paths.map((path, index) => ({ id: `t${index}`, path }));

  it("is a failure with the reason when every tab slot is taken, and queues nothing", async () => {
    // Pre-fix this was "queued": a request that waited for a tab the store
    // would refuse to open for as long as the slots stayed full.
    repoState.set({ currentPath: "/work/current", openTabs: full(Array.from({ length: 24 }, (_, i) => `/other/${i}`)) });
    openRepo.mockResolvedValue(false);
    const outcome = await startTaskTerminal(run("a", "/work/a"));
    expect(outcome).toMatchObject({ kind: "failed", checkout: "/work/a" });
    expect(outcome.kind === "failed" && outcome.reason).toMatch(/Too many open repositories \(max 24\)/);
    expect(get(taskTerminalRequests)).toEqual([]);
    expect(openRepo).not.toHaveBeenCalled();
    await expect(showTaskTerminal(run("a", "/work/a"))).rejects.toThrow(/Could not open a: Too many open repositories/);
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/work/a", mode: "ask", reason: "saved" });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toMatchObject({ outcome: "unavailable" });
  });

  it("is not refused for capacity when the checkout already has a tab", async () => {
    repoState.set({ currentPath: "/work/current", openTabs: full(["/work/a", ...Array.from({ length: 23 }, (_, i) => `/other/${i}`)]) });
    openRepo.mockResolvedValue(true);
    expect(await startTaskTerminal(run("a", "/work/a"))).toEqual({ kind: "started" });
  });
});

describe("a checkout that opened waiting to be trusted", () => {
  it("is what the start waits for, and the trust button asks on its tab", async () => {
    openRepo.mockImplementation(async () => {
      repoState.set({ currentPath: "/work/current", openTabs: [{ id: "tab-a", path: "/work/a", trustRequired: true }] });
      return true;
    });
    expect(await startTaskTerminal(run("a", "/work/a"))).toEqual({ kind: "waiting", reason: "trust", checkout: "/work/a" });
    expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["a"]);
    expect(await trustAttemptCheckout({ cwd: "/work/a" })).toBe(true);
    // Trust is asked for in place: the repository behind the task sheet stays put.
    expect(trustTab).toHaveBeenCalledWith("tab-a");
    expect(activateTab).not.toHaveBeenCalled();
    repoState.set({ currentPath: "/work/current", openTabs: [] });
    expect(await trustAttemptCheckout({ cwd: "/work/a" })).toBe(false);
  });

  it("shows the same answer when the reader asks to see the terminal", async () => {
    repoState.set({ currentPath: "/work/current", openTabs: [{ id: "tab-a", path: "/work/a", trustRequired: true }] });
    openRepo.mockResolvedValue(true);
    expect(await showAttemptTerminal(run("a", "/work/a"))).toEqual({ kind: "waiting", reason: "trust", checkout: "/work/a" });
  });
});

describe("an agent in a worktree of a repository that is already open", () => {
  // Every launch into a fresh worktree used to open that worktree as a
  // repository tab. Those share the reader's own bound (24), so about ten
  // agents in, every further launch failed with "Too many open repositories"
  // while the run and session limits stood mostly unused.
  const FAMILY = "/work/a/.git";
  const wt = (name: string) => `/work/a/.gitpulse/worktrees/${name}`;
  const fullStrip = () => [
    { id: "main", path: "/work/a", family: FAMILY, familyRoot: "/work/a" },
    ...Array.from({ length: 23 }, (_, i) => ({ id: `o${i}`, path: `/other/${i}`, family: `/other/${i}/.git`, familyRoot: `/other/${i}` })),
  ];

  it("starts in that repository's tab without opening one, even with every tab slot taken", async () => {
    repoState.set({ currentPath: "/work/a", openTabs: fullStrip() });
    familyOf.mockResolvedValue(FAMILY);
    for (let i = 0; i < 30; i += 1) {
      expect(await startTaskTerminal(run(`r${i}`, wt(`w${i}`)))).toEqual({ kind: "started" });
    }
    expect(openRepo).not.toHaveBeenCalled();
    const queued = get(taskTerminalRequests);
    expect(queued).toHaveLength(30);
    expect(queued[0]).toEqual({ runId: "r0", repoPath: wt("w0"), provider: "claude", title: "Task r0", family: FAMILY });
  });

  it("shows it in that repository's tab, never the worktree's", async () => {
    repoState.set({ currentPath: "/other/1", openTabs: fullStrip() });
    familyOf.mockResolvedValue(FAMILY);
    openRepo.mockImplementation(async (_path: string, options: { onReady: () => void }) => { options.onReady(); return true; });
    expect(await showAttemptTerminal(run("a", wt("w")))).toEqual({ kind: "opened" });
    expect(openRepo).toHaveBeenCalledTimes(1);
    expect(openRepo.mock.calls[0][0]).toBe("/work/a");
    expect(setTerminalOpen).toHaveBeenCalledWith(true);
  });

  it("resumes a conversation there too, carrying the worktree it ran in", async () => {
    repoState.set({ currentPath: "/work/a", openTabs: fullStrip() });
    familyOf.mockResolvedValue(FAMILY);
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: wt("w"), mode: "edit", reason: "saved" });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "started" });
    expect(openRepo).not.toHaveBeenCalled();
    expect(get(taskTerminalRequests)[0]).toMatchObject({ repoPath: wt("w"), family: FAMILY, resume: { sessionId: SESSION } });
  });

  it("opens the worktree as before when no checkout of its repository is open, or its family is unknown", async () => {
    repoState.set({ currentPath: "/other/1", openTabs: [{ id: "o1", path: "/other/1", family: "/other/1/.git", familyRoot: "/other/1" }] });
    familyOf.mockResolvedValue(FAMILY);
    openRepo.mockResolvedValue(true);
    expect(await startTaskTerminal(run("a", wt("w")))).toEqual({ kind: "started" });
    expect(openRepo).toHaveBeenCalledWith(wt("w"), { activate: false, deferTrust: true });
    expect(get(taskTerminalRequests)[0]).not.toHaveProperty("family");

    openRepo.mockClear();
    repoState.set({ currentPath: "/work/a", openTabs: [{ id: "main", path: "/work/a", family: FAMILY, familyRoot: "/work/a" }] });
    familyOf.mockResolvedValue(null);
    expect(await startTaskTerminal(run("b", wt("v")))).toEqual({ kind: "started" });
    expect(openRepo).toHaveBeenCalledWith(wt("v"), { activate: false, deferTrust: true });
  });

  it("is not hosted by a sibling the reader has not trusted, or one deleted from disk", async () => {
    repoState.set({ currentPath: "/work/a", openTabs: [
      { id: "main", path: "/work/a", family: FAMILY, familyRoot: "/work/a", trustRequired: true },
      { id: "gone", path: wt("old"), family: FAMILY, familyRoot: "/work/a", missing: true },
    ] });
    familyOf.mockResolvedValue(FAMILY);
    openRepo.mockResolvedValue(true);
    await startTaskTerminal(run("a", wt("w")));
    expect(openRepo).toHaveBeenCalledWith(wt("w"), { activate: false, deferTrust: true });
  });

  it("asks nothing about the family when the worktree's own tab is open", async () => {
    repoState.set({ currentPath: "/work/a", openTabs: [{ id: "w", path: wt("w"), family: FAMILY, familyRoot: "/work/a" }] });
    openRepo.mockResolvedValue(true);
    await startTaskTerminal(run("a", wt("w")));
    expect(familyOf).not.toHaveBeenCalled();
  });
});
