import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createSessionLifecycle, type SessionTransport } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";
import type { PtySessionHandlers } from "./ptyBus";
import { DEFAULT_TERMINAL_SESSIONS } from "./sessionLimit";

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
  return { owner, hooks, registry, transport, prepare, readyRelease, unsubscribe, output: (s: string, reserved?: number) => {
    if (typeof reserved === "number") handlers?.onOutput(s, reserved);
    else handlers?.onOutput(s);
  }, error: (message: string) => handlers?.onError?.(message), exit: () => handlers?.onExit({ id: "native-a", exit_code: 0, signal: "", error: null, reaped: true }) };
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
  it("forwards a reserved length and omits it when the event has none", async () => {
    const f = fixture();
    await f.owner.start();
    f.output("QQ==", 1);
    expect(f.hooks.output).toHaveBeenCalledWith("QQ==", "native-a", 1);
    f.output("Qg==");
    expect(f.hooks.output).toHaveBeenLastCalledWith("Qg==", "native-a");
    f.owner.dispose(); await flush();
  });
  it("kills a session whose output credit cannot be measured and keeps the error", async () => {
    const f = fixture();
    await f.owner.start();
    await f.owner.stop("Invalid terminal output");
    expect(f.transport.kill).toHaveBeenCalledWith("native-a");
    expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("Invalid"));
    expect(f.owner.write("after-stop")).toBe(false);
    f.owner.dispose(); await flush();
  });
  it("keeps delivering output after dispose until the process is released", async () => {
    // Native reserve() already counted these bytes. Dropping them here leaves
    // the reader blocked until the stall timeout kills the child, because
    // kill's flow.stop() has not run yet. The view acks; this layer must not
    // swallow the chunk. After release, the session is gone and a late chunk
    // is not a credit the reader is still waiting on.
    const pending = deferred<void>();
    const f = fixture({ kill: () => pending.promise });
    await f.owner.start();
    f.owner.dispose();
    f.output("still-reserved");
    expect(f.hooks.output).toHaveBeenCalledWith("still-reserved", "native-a");
    for (let i = 0; i < 1000; i++) f.output(`c${i}`);
    expect(f.hooks.output).toHaveBeenCalledTimes(1001);
    pending.resolve();
    await flush();
    f.hooks.output.mockClear();
    f.output("after-release");
    expect(f.hooks.output).not.toHaveBeenCalled();
  });
  it("coalesces 10000 resize requests, preserving the final size", async () => {
    const pending = deferred<void>(), resize = vi.fn(() => pending.promise), f = fixture({ resize });
    await f.owner.start(); for (let i = 1; i <= 10000; i++) f.owner.resize(i, i);
    expect(resize).toHaveBeenCalledTimes(1); pending.resolve(); await flush();
    expect(resize).toHaveBeenCalledTimes(2); expect(resize).toHaveBeenLastCalledWith("native-a", 1000, 1000);
    f.owner.dispose(); await flush();
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
