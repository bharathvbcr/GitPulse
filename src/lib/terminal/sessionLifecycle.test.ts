import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createSessionLifecycle, type SessionTransport } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";
import type { PtySessionHandlers } from "./ptyBus";
import { DEFAULT_TERMINAL_SESSIONS } from "./sessionLimit";
import { CONTEXT_READ_MS, KILL_HARD_MS } from "./inputQueue";

type Slot = ReturnType<ReturnType<typeof createSessionRegistry>["reserve"]>;

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { promise, resolve }; }
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
type LifecycleExtras = Pick<Parameters<typeof createSessionLifecycle>[0], "idle" | "keep" | "launcher">;
function fixture(overrides: Partial<SessionTransport> = {}, registry = createSessionRegistry(), key = "tab-a", singleAttempt = false, extras: LifecycleExtras = {}) {
  let handlers: PtySessionHandlers | null = null;
  const readyRelease = vi.fn(), unsubscribe = vi.fn();
  const prepare = vi.fn<() => Promise<() => void>>(async () => readyRelease);
  const transport = {
    spawn: vi.fn(overrides.spawn ?? (async () => ({ id: "native-a", shell: "/bin/sh", cwd: "/repo" }))),
    write: vi.fn(overrides.write ?? (async () => {})), resize: vi.fn(overrides.resize ?? (async () => {})), kill: vi.fn(overrides.kill ?? (async () => {})),
    detach: vi.fn(overrides.detach ?? (async () => true)),
  };
  const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
  const owner = createSessionLifecycle({ key, repoPath: "/repo", label: "Shell", registry, transport, hooks, singleAttempt, ...extras,
    bus: { prepare, pendingCount: () => 0, openOutput: () => ({ channel: {}, bind: () => {}, release: () => {} }), subscribe(_id, h) { handlers = h; return unsubscribe; } },
  });
  return { owner, hooks, registry, transport, prepare, readyRelease, unsubscribe, output: (s: string) => handlers?.onOutput(new TextEncoder().encode(s)), error: (message: string) => handlers?.onError?.(message), notice: (message: string) => handlers?.onNotice?.(message), exit: () => handlers?.onExit({ id: "native-a", exit_code: 0, signal: "", error: null, reaped: true, ended_by_gitpulse: false }) };
}

describe("the session record", () => {
  it("names the launcher, so the Sessions list can tell an agent from a shell", async () => {
    // The label is display text ("Claude"); the kind is what a chip and any
    // other surface decide on, and it was not on the record at all.
    const registry = createSessionRegistry();
    const transport = { spawn: vi.fn(async () => ({ id: "native-l", shell: "/bin/sh", cwd: "/repo" })), write: vi.fn(async () => {}), resize: vi.fn(async () => {}), kill: vi.fn(async () => {}), detach: vi.fn(async () => true) };
    const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
    const owner = createSessionLifecycle({ key: "tab-l", repoPath: "/repo", label: "Claude", launcher: "claude", registry, transport, hooks,
      bus: { prepare: async () => () => {}, pendingCount: () => 0, openOutput: () => ({ channel: {}, bind: () => {}, release: () => {} }), subscribe: () => () => {} },
    });
    await owner.start();
    expect(get(registry)[0]).toMatchObject({ launcher: "claude", label: "Claude" });
    owner.dispose(); await flush();
  });
});

describe("terminal lifecycle races", () => {
  it("reconnects a task attempt without killing its live process or clearing output", async () => {
    const f = fixture({}, createSessionRegistry(), "task", true);
    await f.owner.start();
    f.owner.fail("Transport disconnected");
    await f.owner.restart();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.hooks.reset).not.toHaveBeenCalled();
    expect(f.transport.spawn).toHaveBeenCalledTimes(2);
    expect(f.owner.isCurrent("native-a")).toBe(true);
    expect(f.owner.write("continue\n")).toBe(true);
    expect(get(f.registry)).toHaveLength(1);
    f.owner.dispose(); await flush();
  });
  it("keeps an acknowledgement failure on the clean exit that follows it", async () => {
    const f = fixture();
    await f.owner.start();
    f.owner.fail("Output acknowledgement failed: denied");
    f.exit();
    expect(f.hooks.exit).toHaveBeenCalledWith(expect.objectContaining({
      exit_code: 0,
      error: expect.stringContaining("acknowledgement"),
    }));
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("acknowledgement"));
    f.owner.dispose(); await flush();
  });
  it("shows every live loss together while the shell is still accepting input", async () => {
    const f = fixture();
    await f.owner.start();
    f.error("Terminal output chunk exceeded its limit");
    f.error("Invalid terminal output event");
    f.error("Terminal output chunk exceeded its limit");
    expect(f.owner.write("still-alive")).toBe(true);
    const message = f.hooks.state.mock.calls.at(-1)?.[1] as string;
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("exceeded"));
    expect(message).toContain("Invalid");
    expect(message).toBe("Terminal output chunk exceeded its limit Invalid terminal output event");
    f.owner.dispose(); await flush();
  });
  it("keeps a loss notice when the session is closed without an exit event", async () => {
    const f = fixture();
    await f.owner.start();
    f.error("Terminal output chunk exceeded its limit");
    await get(f.registry)[0].close();
    expect(f.owner.write("after-close")).toBe(false);
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("exceeded"));
    const message = f.hooks.state.mock.calls.at(-1)?.[1] as string;
    expect(message).not.toContain("This session ended");
  });
  it("keeps an acknowledgement failure when the session is closed without an exit event", async () => {
    const f = fixture();
    await f.owner.start();
    f.owner.fail("Output acknowledgement failed: denied");
    await get(f.registry)[0].close();
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("acknowledgement"));
  });
  it("marks a close with no loss as exited", async () => {
    const f = fixture();
    await f.owner.start();
    await get(f.registry)[0].close();
    expect(f.hooks.state).toHaveBeenLastCalledWith("exited");
  });
  it("keeps a live loss when a task reconnects the same process", async () => {
    const f = fixture({}, createSessionRegistry(), "task", true);
    await f.owner.start();
    f.error("Terminal output chunk exceeded its limit");
    await f.owner.restart();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.owner.write("continue\n")).toBe(true);
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("exceeded"));
  });
  it("starts a replacement shell without the previous process's loss", async () => {
    const f = fixture();
    await f.owner.start();
    f.error("Terminal output chunk exceeded its limit");
    await f.owner.restart();
    expect(f.transport.spawn).toHaveBeenCalledTimes(2);
    expect(f.hooks.state).toHaveBeenLastCalledWith("running", undefined);
  });
  it("keeps a retained output failure when a task reconnects the same process", async () => {
    const f = fixture({}, createSessionRegistry(), "task", true);
    await f.owner.start();
    f.owner.fail("Terminal output was dropped: the view was not open", true);
    await f.owner.restart();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.owner.write("continue\n")).toBe(true);
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("dropped"));
  });
  it("clears a transport failure once the same task process is connected again", async () => {
    const f = fixture({}, createSessionRegistry(), "task", true);
    await f.owner.start();
    f.owner.fail("Transport disconnected");
    await f.owner.restart();
    expect(f.owner.write("continue\n")).toBe(true);
    expect(f.hooks.state).toHaveBeenLastCalledWith("running", undefined);
  });
  it("keeps a slow task launch attached to the same attempt without cancelling a late success", async () => {
    vi.useFakeTimers();
    try {
      const pending = deferred<{ id: string; shell: string; cwd: string }>();
      const f = fixture({ spawn: () => pending.promise }, createSessionRegistry(), "task", true);
      const started = f.owner.start(); await flush();
      await vi.advanceTimersByTimeAsync(15000);
      expect(get(f.registry)).toHaveLength(1);
      expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("still pending"));
      pending.resolve({ id: "late", shell: "codex", cwd: "/repo" }); await started;
      expect(f.transport.kill).not.toHaveBeenCalled();
      expect(f.owner.isCurrent("late")).toBe(true);
      expect(f.hooks.started).toHaveBeenCalledTimes(1);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });
  it("retains a live task on failed reconnect and refuses an ended attempt", async () => {
    const f = fixture({}, createSessionRegistry(), "task", true);
    await f.owner.start();
    f.transport.spawn.mockRejectedValueOnce(new Error("Disconnected"));
    await f.owner.restart();
    expect(f.owner.isCurrent("native-a")).toBe(true);
    expect(get(f.registry)).toHaveLength(1);
    expect(f.transport.kill).not.toHaveBeenCalled();
    f.exit();
    await f.owner.restart();
    expect(f.transport.spawn).toHaveBeenCalledTimes(2);
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("attempt ended"));
    f.owner.dispose(); await flush();
  });
  it("rejects a changed process identity during task reconnect without discarding its owner", async () => {
    const f = fixture({}, createSessionRegistry(), "task", true);
    await f.owner.start();
    f.transport.spawn.mockResolvedValueOnce({ id: "other", shell: "codex", cwd: "/repo" });
    await f.owner.restart();
    expect(f.owner.isCurrent("native-a")).toBe(true);
    expect(get(f.registry)).toHaveLength(1);
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("different terminal"));
    expect(f.transport.kill).not.toHaveBeenCalled();
    f.owner.dispose(); await flush();
  });
  it("prepares listeners before spawning and releases the temporary lease", async () => {
    const f = fixture(); await f.owner.start();
    expect(f.prepare.mock.invocationCallOrder[0]).toBeLessThan(f.transport.spawn.mock.invocationCallOrder[0]);
    expect(f.readyRelease).toHaveBeenCalledTimes(1);
    await f.owner.close(); f.owner.dispose(); await flush();
    expect(get(f.registry)).toHaveLength(0);
    expect(f.unsubscribe).toHaveBeenCalledTimes(1);
  });
  it("serializes repeated starts", async () => {
    const pending = deferred<{ id: string; shell: string; cwd: string }>();
    const spawn = vi.fn(() => pending.promise), f = fixture({ spawn });
    const attempts = Array.from({ length: 100 }, () => f.owner.start());
    await flush(); expect(spawn).toHaveBeenCalledTimes(1);
    pending.resolve({ id: "native-a", shell: "sh", cwd: "/repo" }); await Promise.all(attempts);
    f.owner.dispose(); await flush();
  });
  it("retains a timed-out spawn slot and reclaims a late process before retrying", async () => {
    vi.useFakeTimers();
    try {
      const pending = deferred<{ id: string; shell: string; cwd: string }>();
      const f = fixture({ spawn: () => pending.promise });
      const started = f.owner.start(); await flush();
      await vi.advanceTimersByTimeAsync(15000);
      expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("timed out"));
      expect(get(f.registry)).toHaveLength(1);
      expect(f.owner.start()).toBe(started);
      pending.resolve({ id: "late", shell: "sh", cwd: "/repo" }); await started;
      expect(f.transport.kill).toHaveBeenCalledWith("late");
      expect(f.hooks.started).not.toHaveBeenCalled();
      expect(f.owner.isCurrent("late")).toBe(false);
      expect(get(f.registry)).toHaveLength(0);
      await f.owner.start();
      expect(f.transport.spawn).toHaveBeenCalledTimes(2);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });
  it("retains ownership after a close timeout and permits an explicit retry", async () => {
    vi.useFakeTimers();
    try {
      const pending = deferred<void>(), f = fixture({ kill: () => pending.promise });
      await f.owner.start();
      const restart = f.owner.restart();
      await vi.advanceTimersByTimeAsync(KILL_HARD_MS + 500); await restart;
      expect(get(f.registry)).toHaveLength(1);
      expect(f.transport.spawn).toHaveBeenCalledTimes(1);
      pending.resolve(); await get(f.registry)[0].close();
      expect(get(f.registry)).toHaveLength(0);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });
  it("keeps a spawn that completes after disposal, without rendering or journaling it", async () => {
    // It used to be killed. A task tab whose pane remounted mid-launch lost
    // the agent it had just started; the process now goes to Sessions.
    const pending = deferred<{ id: string; shell: string; cwd: string }>();
    const registry = createSessionRegistry();
    const keep = vi.fn(async (id: string) => (slot: Slot) => {
      slot.handOver({ key: `detached:${id}`, repoPath: "/repo", label: "Shell", status: "kept", sessionId: id, close: async () => {} });
    });
    const f = fixture({ spawn: () => pending.promise }, registry, "tab-a", false, { keep });
    const started = f.owner.start();
    await flush(); f.owner.dispose(); pending.resolve({ id: "late", shell: "sh", cwd: "/repo" });
    await started; await flush();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.transport.detach).toHaveBeenCalledWith("late");
    expect(f.hooks.started).not.toHaveBeenCalled();
    expect(get(registry).map((r) => r.key)).toEqual(["detached:late"]);
  });
  it("does not spawn after disposal while listener preparation is pending", async () => {
    const pending = deferred<() => void>(), f = fixture();
    f.prepare.mockImplementation(() => pending.promise);
    const start = f.owner.start(); f.owner.dispose(); pending.resolve(f.readyRelease); await start;
    expect(f.transport.spawn).not.toHaveBeenCalled(); expect(f.readyRelease).toHaveBeenCalledTimes(1);
    expect(get(f.registry)).toHaveLength(0);
  });
  it("keeps a failed close visible with a retry and refuses a replacement process", async () => {
    const kill = vi.fn<() => Promise<void>>(async () => { throw new Error("native unavailable"); }), f = fixture({ kill });
    await f.owner.start(); await f.owner.restart();
    expect(f.transport.spawn).toHaveBeenCalledTimes(1);
    expect(get(f.registry)).toHaveLength(1);
    await expect(f.owner.close()).rejects.toThrow("native unavailable");
    expect(get(f.registry)[0].status).toContain("Close failed");
    kill.mockResolvedValueOnce(undefined); await get(f.registry)[0].close();
    expect(get(f.registry)).toHaveLength(0);
    f.owner.dispose(); await flush();
    expect(f.transport.detach).not.toHaveBeenCalled();
  });
  it("deduplicates rapid restarts and waits for kill before spawn", async () => {
    const pending = deferred<void>(), kill = vi.fn(() => pending.promise), f = fixture({ kill });
    await f.owner.start(); const attempts = Array.from({ length: 100 }, () => f.owner.restart());
    expect(kill).toHaveBeenCalledTimes(1); expect(f.transport.spawn).toHaveBeenCalledTimes(1);
    pending.resolve(); await Promise.all(attempts);
    expect(f.transport.spawn).toHaveBeenCalledTimes(2); expect(f.hooks.reset).toHaveBeenCalledTimes(1);
    f.owner.dispose(); await flush();
  });
  it("releases natural exits and drops late output", async () => {
    const f = fixture(); await f.owner.start(); f.exit(); f.output("late");
    expect(get(f.registry)).toHaveLength(0); expect(f.hooks.output).not.toHaveBeenCalled();
    expect(f.owner.write("should not send")).toBe(false);
    f.owner.dispose(); await flush(); expect(f.transport.kill).not.toHaveBeenCalled();
  });
  it("hands the view each message's bytes with the session they belong to", async () => {
    const f = fixture();
    await f.owner.start();
    f.output("one");
    f.output("two");
    expect(f.hooks.output).toHaveBeenNthCalledWith(1, new TextEncoder().encode("one"), "native-a");
    expect(f.hooks.output).toHaveBeenNthCalledWith(2, new TextEncoder().encode("two"), "native-a");
    f.owner.dispose(); await flush();
  });
  it("reports output whose credit cannot be measured as lost, and keeps the process", async () => {
    const f = fixture();
    await f.owner.start();
    f.owner.fail("Invalid terminal output", true);
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("Invalid"));
    expect(f.owner.isCurrent("native-a")).toBe(true);
    await f.owner.close(); f.owner.dispose(); await flush();
  });
  it("keeps delivering output after dispose until the process is released", async () => {
    // Native reserve() already counted these bytes, and the host has not
    // detached the session yet. Dropping them here would leave the reader
    // waiting until the stall timeout. The view acks; this layer must not
    // swallow the chunk. After release, the session is gone and a late chunk
    // is not a credit the reader is still waiting on.
    const pending = deferred<boolean>();
    const f = fixture({ detach: () => pending.promise });
    await f.owner.start();
    f.owner.dispose();
    await flush();
    f.output("still-reserved");
    expect(f.hooks.output).toHaveBeenCalledWith(new TextEncoder().encode("still-reserved"), "native-a");
    for (let i = 0; i < 1000; i++) f.output(`c${i}`);
    expect(f.hooks.output).toHaveBeenCalledTimes(1001);
    // The session ended while detaching: nothing is kept, and its exit
    // (still delivered here) is what releases it.
    pending.resolve(false);
    await flush();
    expect(get(f.registry)).toHaveLength(1);
    f.exit();
    expect(get(f.registry)).toHaveLength(0);
    f.hooks.output.mockClear();
    f.output("after-release");
    expect(f.hooks.output).not.toHaveBeenCalled();
    expect(f.transport.kill).not.toHaveBeenCalled();
  });
  it("coalesces 10000 resize requests, preserving the final size", async () => {
    const pending = deferred<void>(), resize = vi.fn(() => pending.promise), f = fixture({ resize });
    await f.owner.start(); for (let i = 1; i <= 10000; i++) f.owner.resize(i, i);
    expect(resize).toHaveBeenCalledTimes(1); pending.resolve(); await flush();
    expect(resize).toHaveBeenCalledTimes(2); expect(resize).toHaveBeenLastCalledWith("native-a", 1000, 1000);
    f.owner.dispose(); await flush();
  });
  it("tells the view that typing is queued behind a slow answer, retracts it, and never fails the session", async () => {
    vi.useFakeTimers();
    try {
      const pending = deferred<void>(), write = vi.fn(() => pending.promise), f = fixture({ write });
      const slow = vi.fn();
      (f.hooks as { slow?: (message: string | null) => void }).slow = slow;
      await f.owner.start();
      expect(f.owner.write("ls\n")).toBe(true);
      await vi.advanceTimersByTimeAsync(5500); await flush();
      expect(slow).toHaveBeenCalledTimes(1);
      expect(slow).toHaveBeenLastCalledWith(expect.stringContaining("slowly"));
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      pending.resolve(); await flush();
      expect(slow).toHaveBeenCalledTimes(2);
      expect(slow).toHaveBeenLastCalledWith(null);
      expect(f.owner.write("pwd\n")).toBe(true);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });
  describe("resize on a slow host", () => {
    it("coalesces 50 resizes behind a resize that answers late: two calls, no warning, the latest size wins", async () => {
      // The defect: a resize merely slower than the old 5 s wall-clock deadline
      // was reported as "Resize failed: Error: Terminal resize timed out", once
      // per resize event, so dragging a window on a loaded host spammed it.
      vi.useFakeTimers();
      try {
        const pending = deferred<void>(), resize = vi.fn(() => pending.promise), f = fixture({ resize });
        await f.owner.start();
        for (let i = 1; i <= 50; i++) f.owner.resize(10 + i, 20 + i);
        await vi.advanceTimersByTimeAsync(6000); await flush();
        expect(f.hooks.warning).not.toHaveBeenCalled();
        expect(resize).toHaveBeenCalledTimes(1);
        for (let i = 51; i <= 60; i++) f.owner.resize(10 + i, 20 + i);
        pending.resolve(); await flush();
        expect(resize).toHaveBeenCalledTimes(2);
        expect(resize).toHaveBeenLastCalledWith("native-a", 70, 80);
        expect(f.hooks.warning).not.toHaveBeenCalled();
        f.owner.dispose(); await flush();
      } finally { vi.useRealTimers(); }
    });
    it("warns once for a streak of rejected resizes and again only after one succeeded", async () => {
      let outcome: "reject" | "ok" = "reject";
      const resize = vi.fn(async () => { if (outcome === "reject") throw new Error("pty busy"); });
      const f = fixture({ resize });
      await f.owner.start();
      for (let i = 1; i <= 5; i++) { f.owner.resize(i, i); await flush(); }
      expect(resize).toHaveBeenCalledTimes(5);
      expect(f.hooks.warning).toHaveBeenCalledTimes(1);
      expect(f.hooks.warning).toHaveBeenCalledWith(expect.stringContaining("Resize failed"));
      outcome = "ok"; f.owner.resize(6, 6); await flush();
      expect(f.hooks.warning).toHaveBeenCalledTimes(1);
      outcome = "reject"; f.owner.resize(7, 7); await flush(); f.owner.resize(8, 8); await flush();
      expect(f.hooks.warning).toHaveBeenCalledTimes(2);
      f.owner.dispose(); await flush();
    });
    it("is silent when the session is gone: the exit path reports that", async () => {
      const resize = vi.fn(async () => { throw new Error("Terminal session 'native-a' not found"); });
      const f = fixture({ resize });
      await f.owner.start();
      for (let i = 1; i <= 3; i++) { f.owner.resize(i, i); await flush(); }
      expect(resize).toHaveBeenCalledTimes(3);
      expect(f.hooks.warning).not.toHaveBeenCalled();
      f.owner.dispose(); await flush();
    });
    it("a resize with no answer at all is abandoned at the hard bound, so a later session can resize", async () => {
      // `resizing` is the lifecycle's, not one session's. A resize that never
      // answers must not hold it for ever or the next process never resizes.
      vi.useFakeTimers();
      try {
        const resize = vi.fn(() => (resize.mock.calls.length === 1 ? new Promise<void>(() => {}) : Promise.resolve()));
        const f = fixture({ resize });
        await f.owner.start();
        f.owner.resize(30, 90);
        await vi.advanceTimersByTimeAsync(59_000); await flush();
        expect(f.hooks.warning).not.toHaveBeenCalled();
        f.owner.resize(31, 91);
        await vi.advanceTimersByTimeAsync(2_000); await flush();
        expect(resize).toHaveBeenCalledTimes(2);
        expect(resize).toHaveBeenLastCalledWith("native-a", 31, 91);
        expect(f.hooks.warning).toHaveBeenCalledTimes(1);
        expect(f.hooks.warning).toHaveBeenCalledWith(expect.stringContaining("no answer"));
        f.owner.dispose(); await flush();
      } finally { vi.useRealTimers(); }
    });
  });
  it("enforces the global slot ceiling across 100 concurrent repository starts", async () => {
    const registry = createSessionRegistry();
    // Each process its own native id, as the backend issues them: the
    // registry keeps one record per native session, so 100 sessions sharing
    // one id would be one session, not a ceiling test.
    const sessions = Array.from({ length: 100 }, (_, i) =>
      fixture({ spawn: async () => ({ id: `native-${i}`, shell: "/bin/sh", cwd: "/repo" }) }, registry, String(i)));
    await Promise.all(sessions.map((f) => f.owner.start()));
    expect(get(registry)).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
    expect(sessions.reduce((n, f) => n + f.transport.spawn.mock.calls.length, 0)).toBe(DEFAULT_TERMINAL_SESSIONS);
    await Promise.all(sessions.map((f) => f.owner.close())); for (const f of sessions) f.owner.dispose();
    await flush(); expect(get(registry)).toHaveLength(0);
  });
});

it("waits for the old renderer to drain before starting a replacement", async () => {
  const f = fixture(); await f.owner.start();
  const drained = deferred<void>();
  f.hooks.reset.mockImplementation(() => drained.promise);
  const restarted = f.owner.restart(); await flush();
  expect(f.transport.spawn).toHaveBeenCalledTimes(1);
  drained.resolve(); await restarted;
  expect(f.transport.spawn).toHaveBeenCalledTimes(2);
  f.owner.dispose(); await flush();
});

describe("a hosted session's record", () => {
  it("keeps the host as repoPath and carries the checkout it runs in", async () => {
    const registry = createSessionRegistry();
    const transport = { spawn: vi.fn(async () => ({ id: "native-h", shell: "claude", cwd: "/repo/.gitpulse/worktrees/a" })), write: vi.fn(async () => {}), resize: vi.fn(async () => {}), kill: vi.fn(async () => {}), detach: vi.fn(async () => true) };
    const owner = createSessionLifecycle({ key: "tab-h", repoPath: "/repo", checkout: "/repo/.gitpulse/worktrees/a", label: "Claude", registry, transport,
      hooks: { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() },
      bus: { prepare: async () => () => {}, pendingCount: () => 0, openOutput: () => ({ channel: {}, bind: () => {}, release: () => {} }), subscribe: () => () => {} },
    });
    await owner.start();
    expect(get(registry)[0]).toMatchObject({ repoPath: "/repo", checkout: "/repo/.gitpulse/worktrees/a" });
    owner.dispose(); await flush();
  });
});

describe("admission to a session slot", () => {
  function owner(registry: ReturnType<typeof createSessionRegistry>, key: string, hooks: { admitted?: () => void; refused?: (m: string) => boolean; closed?: () => void }) {
    const transport = { spawn: vi.fn(async () => ({ id: `native-${key}`, shell: "/bin/sh", cwd: "/repo" })), write: vi.fn(async () => {}), resize: vi.fn(async () => {}), kill: vi.fn(async () => {}), detach: vi.fn(async () => true) };
    const state = vi.fn();
    const lifecycle = createSessionLifecycle({ key, repoPath: "/repo", label: "Claude", registry, transport,
      hooks: { state, started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn(), ...hooks },
      bus: { prepare: async () => () => {}, pendingCount: () => 0, openOutput: () => ({ channel: {}, bind: () => {}, release: () => {} }), subscribe: () => () => {} },
    });
    return { lifecycle, transport, state };
  }

  it("says once when a tab takes a slot, before anything spawns", async () => {
    const registry = createSessionRegistry();
    const order: string[] = [];
    const a = owner(registry, "a", { admitted: () => order.push("admitted") });
    a.transport.spawn.mockImplementation(async () => { order.push("spawn"); return { id: "native-a", shell: "/bin/sh", cwd: "/repo" }; });
    await a.lifecycle.start();
    expect(order).toEqual(["admitted", "spawn"]);
    a.lifecycle.dispose(); await flush();
  });

  // S2-04: a relaunch counted as "resumed" must be one that started.
  it("restart says what it did: started, deferred behind a full registry, failed on a rejecting transport", async () => {
    const registry = createSessionRegistry();
    const ok = owner(registry, "ok", {});
    await ok.lifecycle.start();
    expect(await ok.lifecycle.restart()).toBe("started");

    const broken = owner(registry, "broken", {});
    broken.transport.spawn.mockImplementation(async () => { throw new Error("CLI missing"); });
    expect(await broken.lifecycle.restart()).toBe("failed");
    expect(broken.state).toHaveBeenCalledWith("error", expect.stringContaining("CLI missing"));

    ok.lifecycle.dispose(); broken.lifecycle.dispose(); await flush();
    const full = createSessionRegistry();
    const held = Array.from({ length: DEFAULT_TERMINAL_SESSIONS }, (_, i) => full.reserve({ key: `held-${i}`, repoPath: "/repo", label: "Shell", status: "running", close: async () => {} }));
    const waiting = owner(full, "waiting", { refused: () => true });
    expect(await waiting.lifecycle.restart()).toBe("deferred");
    expect(waiting.transport.spawn).not.toHaveBeenCalled();
    for (const slot of held) slot.release();
    waiting.lifecycle.dispose(); await flush();
  });

  it("an acknowledged close is reported once, even after the view went away", async () => {
    const registry = createSessionRegistry();
    const closed = vi.fn();
    const tab = owner(registry, "c", { closed });
    await tab.lifecycle.start();
    await tab.lifecycle.close();
    expect(closed).toHaveBeenCalledTimes(1);

    const gone = owner(registry, "g", { closed });
    await gone.lifecycle.start();
    const closing = gone.lifecycle.close();
    gone.lifecycle.dispose();
    await closing; await flush();
    expect(closed).toHaveBeenCalledTimes(2);
  });

  it("hands a refusal for capacity back to a caller that will retry, and starts nothing", async () => {
    const registry = createSessionRegistry();
    const held = Array.from({ length: DEFAULT_TERMINAL_SESSIONS }, (_, i) => registry.reserve({ key: `held-${i}`, repoPath: "/repo", label: "Shell", status: "running", close: async () => {} }));
    const refused = vi.fn(() => true), admitted = vi.fn();
    const late = owner(registry, "late", { refused, admitted });
    await late.lifecycle.start();
    expect(refused).toHaveBeenCalledWith(`All ${DEFAULT_TERMINAL_SESSIONS} terminal sessions are in use across repositories`);
    expect(admitted).not.toHaveBeenCalled();
    expect(late.transport.spawn).not.toHaveBeenCalled();
    expect(late.state).not.toHaveBeenCalledWith("error", expect.anything());
    expect(get(registry)).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
    for (const slot of held) slot.release();
    late.lifecycle.dispose(); await flush();
  });

  it("still reports the refusal as the tab's error to a caller that will not retry", async () => {
    const registry = createSessionRegistry();
    const held = Array.from({ length: DEFAULT_TERMINAL_SESSIONS }, (_, i) => registry.reserve({ key: `held-${i}`, repoPath: "/repo", label: "Shell", status: "running", close: async () => {} }));
    for (const hooks of [{}, { refused: () => false }]) {
      const late = owner(registry, "late", hooks);
      await late.lifecycle.start();
      expect(late.state).toHaveBeenCalledWith("error", expect.stringContaining("terminal sessions are in use"));
      expect(late.transport.spawn).not.toHaveBeenCalled();
      late.lifecycle.dispose(); await flush();
    }
    for (const slot of held) slot.release();
  });
});

/**
 * The reported defect: an agent's tab unmounting for any reason other than
 * Close — its repository tab closed, its pane crashed, the dock re-hosted —
 * killed the agent, and its work was lost.
 */
describe("a view that goes away without Close", () => {
  function keeper() {
    const adopted: string[] = [];
    const keep = vi.fn(async (id: string) => (slot: Slot) => {
      slot.handOver({ key: `detached:${id}`, repoPath: "/repo", label: "Claude", status: "Still running", sessionId: id, close: async () => {} });
      adopted.push(id);
    });
    return { keep, adopted };
  }

  it("detaches a running agent and hands its slot to Sessions; it is never killed", async () => {
    const registry = createSessionRegistry();
    const { keep, adopted } = keeper();
    const f = fixture({}, registry, "tab-a", false, { keep, launcher: "claude" });
    await f.owner.start();
    // The slot must never be observably free during the hand-over: another
    // tab could take it, and the adopted record would then be refused.
    const sizes: number[] = [];
    const stop = registry.subscribe((records) => sizes.push(records.length));
    f.owner.dispose(); await flush();
    stop();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.transport.detach).toHaveBeenCalledWith("native-a");
    expect(adopted).toEqual(["native-a"]);
    expect(get(registry).map((r) => r.key)).toEqual(["detached:native-a"]);
    expect(Math.min(...sizes)).toBe(1);
    expect(f.unsubscribe).toHaveBeenCalledTimes(1);
  });

  it("ends a plain shell positively idle at its prompt, so empty shells do not pile up", async () => {
    const registry = createSessionRegistry();
    const { keep } = keeper();
    const idle = vi.fn(async () => true);
    const f = fixture({}, registry, "tab-a", false, { keep, idle });
    await f.owner.start();
    f.owner.dispose(); await flush();
    expect(idle).toHaveBeenCalledWith("native-a");
    expect(f.transport.kill).toHaveBeenCalledWith("native-a");
    expect(f.transport.detach).not.toHaveBeenCalled();
    expect(get(registry)).toHaveLength(0);
  });

  it("keeps the process when idleness cannot be read: unknown is not idle", async () => {
    for (const idle of [vi.fn(async () => { throw new Error("context failed"); }), vi.fn(() => new Promise<boolean>(() => {}))]) {
      vi.useFakeTimers();
      try {
        const registry = createSessionRegistry();
        const { keep } = keeper();
        const f = fixture({}, registry, "tab-a", false, { keep, idle });
        await f.owner.start();
        f.owner.dispose();
        await vi.advanceTimersByTimeAsync(CONTEXT_READ_MS + 500); await flush();
        expect(f.transport.kill).not.toHaveBeenCalled();
        expect(f.transport.detach).toHaveBeenCalledWith("native-a");
        expect(get(registry).map((r) => r.key)).toEqual(["detached:native-a"]);
      } finally { vi.useRealTimers(); }
    }
  });

  it("keeps ownership, and says so, when the host cannot detach", async () => {
    const registry = createSessionRegistry();
    const { keep, adopted } = keeper();
    const f = fixture({ detach: async () => { throw new Error("host busy"); } }, registry, "tab-a", false, { keep });
    await f.owner.start();
    f.owner.dispose(); await flush();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(adopted).toEqual([]);
    expect(get(registry)).toHaveLength(1);
    expect(get(registry)[0].status).toContain("could not be detached");
    // Still stoppable from Sessions.
    await get(registry)[0].close();
    expect(f.transport.kill).toHaveBeenCalledWith("native-a");
    expect(get(registry)).toHaveLength(0);
  });

  it("stays the owner when there is nothing to hand over to", async () => {
    for (const keep of [undefined, vi.fn(async () => null), vi.fn(async () => { throw new Error("list failed"); })]) {
      const registry = createSessionRegistry();
      const f = fixture({}, registry, "tab-a", false, keep ? { keep } : {});
      await f.owner.start();
      f.owner.dispose(); await flush();
      expect(f.transport.kill).not.toHaveBeenCalled();
      expect(get(registry)).toHaveLength(1);
      expect(get(registry)[0].status).toContain("Still running");
      f.exit();
      expect(get(registry)).toHaveLength(0);
    }
  });

  it("still hears the exit of a process spawned after its view went away, when nothing took it over", async () => {
    const pending = deferred<{ id: string; shell: string; cwd: string }>();
    const registry = createSessionRegistry();
    const f = fixture({ spawn: () => pending.promise }, registry, "tab-a", false, { keep: async () => null });
    const started = f.owner.start();
    await flush(); f.owner.dispose(); pending.resolve({ id: "native-a", shell: "sh", cwd: "/repo" });
    await started; await flush();
    expect(f.transport.kill).not.toHaveBeenCalled();
    expect(f.hooks.started).not.toHaveBeenCalled();
    expect(get(registry)[0].status).toContain("Still running");
    f.exit();
    expect(get(registry)).toHaveLength(0);
  });

  it("adopts nothing when the process exits while it is being detached", async () => {
    const registry = createSessionRegistry();
    const { keep, adopted } = keeper();
    const pending = deferred<boolean>();
    const f = fixture({ detach: () => pending.promise }, registry, "tab-a", false, { keep });
    await f.owner.start();
    f.owner.dispose(); await flush();
    f.exit();
    pending.resolve(true); await flush();
    expect(adopted).toEqual([]);
    expect(get(registry)).toHaveLength(0);
    expect(f.transport.kill).not.toHaveBeenCalled();
  });

  it("lets a Close already under way finish rather than detach behind it", async () => {
    const registry = createSessionRegistry();
    const { keep, adopted } = keeper();
    const pending = deferred<void>();
    const f = fixture({ kill: () => pending.promise }, registry, "tab-a", false, { keep });
    await f.owner.start();
    const closing = f.owner.close();
    f.owner.dispose(); await flush();
    pending.resolve(); await closing; await flush();
    expect(f.transport.detach).not.toHaveBeenCalled();
    expect(adopted).toEqual([]);
    expect(get(registry)).toHaveLength(0);
  });

  it("an explicit Close still ends the process", async () => {
    const f = fixture();
    await f.owner.start();
    await f.owner.close();
    expect(f.transport.kill).toHaveBeenCalledWith("native-a");
    expect(get(f.registry)).toHaveLength(0);
    f.owner.dispose(); await flush();
    expect(f.transport.detach).not.toHaveBeenCalled();
  });

  it("shows skipped output as a warning; the session keeps its running status", async () => {
    const f = fixture();
    await f.owner.start();
    f.hooks.state.mockClear();
    const skipped = vi.fn();
    (f.hooks as { skipped?: () => void }).skipped = skipped;
    f.notice("Terminal output was skipped while this window was not keeping up");
    expect(skipped).toHaveBeenCalledTimes(1);
    expect(f.hooks.warning).toHaveBeenCalledWith(expect.stringContaining("skipped"));
    expect(f.hooks.state).not.toHaveBeenCalled();
    expect(get(f.registry)[0].status).toBe("running");
    await f.owner.close(); f.owner.dispose(); await flush();
  });

  it("survives a storm: every tab at the session limit disposed at once, none killed, no slot leaked", async () => {
    const registry = createSessionRegistry();
    const { keep, adopted } = keeper();
    const tabs = Array.from({ length: DEFAULT_TERMINAL_SESSIONS }, (_, i) =>
      fixture({ spawn: async () => ({ id: `native-${i}`, shell: "/bin/sh", cwd: "/repo" }) }, registry, `t${i}`, false, { keep, launcher: "claude" }));
    await Promise.all(tabs.map((t) => t.owner.start()));
    expect(get(registry)).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
    // A new tab racing the hand-overs must be refused: no slot is ever free.
    const racer = fixture({}, registry, "racer");
    for (const t of tabs) t.owner.dispose();
    const raced = racer.owner.start();
    await flush(); await raced;
    expect(racer.transport.spawn).not.toHaveBeenCalled();
    expect(tabs.every((t) => t.transport.kill.mock.calls.length === 0)).toBe(true);
    expect(adopted).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
    expect(get(registry)).toHaveLength(DEFAULT_TERMINAL_SESSIONS);
    expect(get(registry).every((r) => r.key.startsWith("detached:"))).toBe(true);
    racer.owner.dispose(); await flush();
  });
});
