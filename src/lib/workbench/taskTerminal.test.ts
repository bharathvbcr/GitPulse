import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const openRepo = vi.fn();
const setTerminalOpen = vi.fn();
const setGlobalSurface = vi.fn();
vi.mock("../stores/repoStore", () => ({ repoStore: { openRepo: (...args: unknown[]) => openRepo(...args), setTerminalOpen: (...args: unknown[]) => setTerminalOpen(...args) } }));
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setGlobalSurface: (...args: unknown[]) => setGlobalSurface(...args) } }));

const { openTaskTerminal, queuedTerminalNote } = await import("./taskTerminal");
const { consumeTaskTerminal, taskTerminalRequests } = await import("../terminal/taskLaunches");

const run = (id: string, cwd: string) => ({ id, cwd, provider: "claude" as const, task_title: `Task ${id}` });

beforeEach(() => { openRepo.mockReset(); setTerminalOpen.mockReset(); setGlobalSurface.mockReset(); });
afterEach(() => { for (const request of get(taskTerminalRequests)) consumeTaskTerminal(request.runId); });

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
