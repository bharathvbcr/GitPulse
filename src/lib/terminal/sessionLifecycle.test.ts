import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createSessionLifecycle, type SessionTransport } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";
import type { PtySessionHandlers } from "./ptyBus";

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { promise, resolve }; }
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
function fixture(overrides: Partial<SessionTransport> = {}, registry = createSessionRegistry(), key = "tab-a", singleAttempt = false) {
  let handlers: PtySessionHandlers | null = null;
  const readyRelease = vi.fn(), unsubscribe = vi.fn();
  const prepare = vi.fn<() => Promise<() => void>>(async () => readyRelease);
  const transport = {
    spawn: vi.fn(overrides.spawn ?? (async () => ({ id: "native-a", shell: "/bin/sh", cwd: "/repo" }))),
    write: vi.fn(overrides.write ?? (async () => {})), resize: vi.fn(overrides.resize ?? (async () => {})), kill: vi.fn(overrides.kill ?? (async () => {})),
  };
  const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
  const owner = createSessionLifecycle({ key, repoPath: "/repo", label: "Shell", registry, transport, hooks, singleAttempt,
    bus: { prepare, pendingCount: () => 0, subscribe(_id, h) { handlers = h; return unsubscribe; } },
  });
  return { owner, hooks, registry, transport, prepare, readyRelease, unsubscribe, output: (s: string) => handlers?.onOutput(s), exit: () => handlers?.onExit({ id: "native-a", exit_code: 0, signal: "", error: null, reaped: true }) };
}

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
    f.owner.dispose(); await flush();
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
      await vi.advanceTimersByTimeAsync(5000); await restart;
      expect(get(f.registry)).toHaveLength(1);
      expect(f.transport.spawn).toHaveBeenCalledTimes(1);
      pending.resolve(); await get(f.registry)[0].close();
      expect(get(f.registry)).toHaveLength(0);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });
  it("reclaims a spawn that completes after disposal without rendering or journaling it", async () => {
    const pending = deferred<{ id: string; shell: string; cwd: string }>();
    const f = fixture({ spawn: () => pending.promise }); const started = f.owner.start();
    await flush(); f.owner.dispose(); pending.resolve({ id: "late", shell: "sh", cwd: "/repo" });
    await started; await flush();
    expect(f.transport.kill).toHaveBeenCalledWith("late");
    expect(f.hooks.started).not.toHaveBeenCalled();
    expect(get(f.registry)).toHaveLength(0);
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
    f.owner.dispose(); await flush();
    expect(get(f.registry)[0].status).toContain("Close failed");
    kill.mockResolvedValueOnce(undefined); await get(f.registry)[0].close();
    expect(get(f.registry)).toHaveLength(0);
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
  it("coalesces 10000 resize requests, preserving the final size", async () => {
    const pending = deferred<void>(), resize = vi.fn(() => pending.promise), f = fixture({ resize });
    await f.owner.start(); for (let i = 1; i <= 10000; i++) f.owner.resize(i, i);
    expect(resize).toHaveBeenCalledTimes(1); pending.resolve(); await flush();
    expect(resize).toHaveBeenCalledTimes(2); expect(resize).toHaveBeenLastCalledWith("native-a", 1000, 1000);
    f.owner.dispose(); await flush();
  });
  it("enforces 16 global slots across 100 concurrent repository starts", async () => {
    const registry = createSessionRegistry();
    const sessions = Array.from({ length: 100 }, (_, i) => fixture({}, registry, String(i)));
    await Promise.all(sessions.map((f) => f.owner.start()));
    expect(get(registry)).toHaveLength(16);
    expect(sessions.reduce((n, f) => n + f.transport.spawn.mock.calls.length, 0)).toBe(16);
    for (const f of sessions) f.owner.dispose(); await flush(); expect(get(registry)).toHaveLength(0);
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
