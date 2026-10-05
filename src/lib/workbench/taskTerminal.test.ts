import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const openRepo = vi.fn();
const setTerminalOpen = vi.fn();
const setGlobalSurface = vi.fn();
vi.mock("../stores/repoStore", () => ({ repoStore: { openRepo: (...args: unknown[]) => openRepo(...args), setTerminalOpen: (...args: unknown[]) => setTerminalOpen(...args) } }));
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: (...args: unknown[]) => setGlobalSurface(...args) } }));
const findConversation = vi.fn();
vi.mock("./client", () => ({ findConversation: (...args: unknown[]) => findConversation(...args) }));

const { openTaskTerminal, queuedTerminalNote, resumeTaskConversation } = await import("./taskTerminal");
const { consumeTaskTerminalRequest, taskTerminalRequests } = await import("../terminal/taskLaunches");

const run = (id: string, cwd: string) => ({ id, cwd, provider: "claude" as const, task_title: `Task ${id}` });

beforeEach(() => { openRepo.mockReset(); setTerminalOpen.mockReset(); setGlobalSurface.mockReset(); findConversation.mockReset(); });
afterEach(() => { for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request); });

describe("openTaskTerminal", () => {
  it("queues the request before the checkout opens, then shows the dock", async () => {
    openRepo.mockImplementation(async (_path: string, options: { onReady: (path: string) => void }) => {
      // The request is already waiting when the dock could first mount.
      expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["a"]);
      options.onReady("/work/a");
      return true;
    });
    expect(await openTaskTerminal(run("a", "/work/a"))).toBe("opened");
    expect(setTerminalOpen).toHaveBeenCalledWith(true);
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
  });

  it("keeps a superseded open's terminal queued instead of throwing it away", async () => {
    // Launching task B while task A's checkout is still opening cancels A's
    // open: its ready callback never runs. That used to throw and strand A.
    let release: (value: boolean) => void = () => {};
    openRepo.mockImplementationOnce(() => new Promise<boolean>((resolve) => { release = resolve; }));
    openRepo.mockImplementationOnce(async (_path: string, options: { onReady: () => void }) => { options.onReady(); return true; });
    const first = openTaskTerminal(run("a", "/work/a"));
    const second = openTaskTerminal(run("b", "/work/b"));
    release(false);
    expect(await first).toBe("queued");
    expect(await second).toBe("opened");
    expect(get(taskTerminalRequests).map((r) => r.runId)).toEqual(["a", "b"]);
    // The dock for /work/a still finds its request whenever it mounts.
    expect(get(taskTerminalRequests)[0]).toMatchObject({ repoPath: "/work/a", title: "Task a" });
  });

  it("treats an open that reports success without becoming ready as queued", async () => {
    openRepo.mockResolvedValueOnce(true);
    expect(await openTaskTerminal(run("c", "/work/c"))).toBe("queued");
    expect(setTerminalOpen).not.toHaveBeenCalled();
  });

  it("says where the queued terminal will appear", () => {
    expect(queuedTerminalNote("/work/repo/.gitpulse/worktrees/fix-a1b2c3d4")).toContain("fix-a1b2c3d4's terminal dock");
    expect(queuedTerminalNote("C:\\work\\repo\\")).toContain("repo's terminal dock");
  });
});

describe("resumeTaskConversation", () => {
  const SESSION = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
  it("reopens the saved conversation where the host found it, in the attempt's mode", async () => {
    findConversation.mockResolvedValueOnce({ resumable: true, sessionId: SESSION, cwd: "/work/a/.gitpulse/worktrees/fix-1", mode: "inspect", reason: "saved" });
    openRepo.mockImplementation(async (_path: string, options: { onReady: () => void }) => { options.onReady(); return true; });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "opened" });
    expect(openRepo).toHaveBeenCalledWith("/work/a/.gitpulse/worktrees/fix-1", expect.anything());
    expect(get(taskTerminalRequests)).toEqual([
      { runId: "a", repoPath: "/work/a/.gitpulse/worktrees/fix-1", provider: "claude", title: "Fix", resume: { sessionId: SESSION, mode: "inspect" } },
    ]);
  });

  it("opens nothing when there is nothing to resume, and says why", async () => {
    findConversation.mockResolvedValueOnce({ resumable: false, reason: "Claude Code has no saved conversation for this attempt." });
    expect(await resumeTaskConversation({ id: "a", task_title: "Fix" })).toEqual({ outcome: "unavailable", reason: "Claude Code has no saved conversation for this attempt." });
    expect(openRepo).not.toHaveBeenCalled();
    expect(get(taskTerminalRequests)).toEqual([]);
  });
});
