import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createSessionLifecycle, type SessionTransport } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";
import { listingFamily } from "./detachedSessions";
import {
  CLOSE_SLOW_NOTICE, CONTEXT_READ_MS, CONTROL_QUICK_MS, FAMILY_LOOKUP_MS, KEEP_MS, KILL_HARD_MS, KILL_SOFT_MS, RENDERER_DRAIN_MS,
} from "./inputQueue";

/**
 * The slower terminal commands (close, detach, the context read, the session
 * listing, the renderer drain) on a host that answers them late. The backend's
 * own kill takes up to ~10 s on a loaded host (the arithmetic is in
 * `inputQueue.ts`), so a frontend bound near 5 s turned a close that worked
 * into "Close failed; use Sessions to retry".
 */

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { promise, resolve }; }
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
const later = async (ms: number) => { await vi.advanceTimersByTimeAsync(ms); await flush(); };

/**
 * A clock the test can stall: `block` models a blocked JS thread (wall time
 * passes, nothing the page can read moves until it is released). The same
 * helper `inputQueue.test.ts` uses, kept local to its file there.
 */
function stallableClock() {
  const clock = {
    frozenAt: null as number | null,
    hiddenNow: false,
    now: () => clock.frozenAt ?? performance.now(),
    hidden: () => clock.hiddenNow,
    async block(ms: number) {
      clock.frozenAt = performance.now();
      await vi.advanceTimersByTimeAsync(ms);
      clock.frozenAt = null;
    },
  };
  return clock;
}

type Slot = ReturnType<ReturnType<typeof createSessionRegistry>["reserve"]>;
type Extras = Partial<Pick<Parameters<typeof createSessionLifecycle>[0], "idle" | "keep" | "clock">>;

function fixture(overrides: Partial<SessionTransport> = {}, extras: Extras = {}) {
  const registry = createSessionRegistry();
  const transport = {
    spawn: vi.fn(overrides.spawn ?? (async () => ({ id: "native-a", shell: "/bin/sh", cwd: "/repo" }))),
    write: vi.fn(async () => {}), resize: vi.fn(async () => {}),
    kill: vi.fn(overrides.kill ?? (async () => {})),
    detach: vi.fn(overrides.detach ?? (async () => true)),
  };
  const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn(), slow: vi.fn() };
  const owner = createSessionLifecycle({
    key: "tab-a", repoPath: "/repo", label: "Shell", registry, transport, hooks, ...extras,
    bus: { prepare: async () => () => {}, pendingCount: () => 0, openOutput: () => ({ channel: {}, bind: () => {}, release: () => {} }), subscribe: () => () => {} },
  });
  return { owner, registry, transport, hooks };
}

function keeper() {
  const adopted: string[] = [];
  const keep = vi.fn(async (id: string) => (slot: Slot) => {
    slot.handOver({ key: `detached:${id}`, repoPath: "/repo", label: "Shell", status: "Still running", sessionId: id, close: async () => {} });
    adopted.push(id);
  });
  return { keep, adopted };
}

describe("a close on a slow host", () => {
  it("a kill that answers at 6 s is a successful close, not 'Close failed; use Sessions to retry'", async () => {
    // The reported defect, with nothing else asserted: the backend's kill
    // (hangup grace, reap, cleanup poll) takes 2.6 to 3.5 s routinely and up to
    // ~10 s under load, and the frontend gave up at 5 s.
    vi.useFakeTimers();
    try {
      const pending = deferred<void>(), f = fixture({ kill: () => pending.promise });
      await f.owner.start();
      const closing = f.owner.close();
      await later(6000);
      pending.resolve(); await closing;
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.stringContaining("Close failed"));
      expect(get(f.registry)).toHaveLength(0);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });

  it("says a slow close is slow, in the view and in Sessions, and retracts it when the kill answers", async () => {
    vi.useFakeTimers();
    try {
      const pending = deferred<void>(), f = fixture({ kill: () => pending.promise });
      await f.owner.start();
      const closing = f.owner.close();
      await later(KILL_SOFT_MS - 500);
      expect(f.hooks.slow).not.toHaveBeenCalled();
      await later(1000);
      expect(f.hooks.slow).toHaveBeenCalledWith(CLOSE_SLOW_NOTICE);
      expect(get(f.registry)[0].status).toBe(CLOSE_SLOW_NOTICE);
      pending.resolve(); await closing;
      expect(f.hooks.slow).toHaveBeenLastCalledWith(null);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      expect(f.hooks.state).toHaveBeenLastCalledWith("exited");
      expect(get(f.registry)).toHaveLength(0);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });

  it("a kill with no answer keeps the slot and the process until the hard bound, then says Close failed", async () => {
    vi.useFakeTimers();
    try {
      const f = fixture({ kill: () => new Promise<void>(() => {}) });
      await f.owner.start();
      const closing = f.owner.close().catch((error: unknown) => error);
      await later(KILL_HARD_MS - 1000);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      await later(2000);
      expect(await closing).toBeInstanceOf(Error);
      expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("Close failed"));
      expect(get(f.registry)).toHaveLength(1);
      expect(f.hooks.slow).toHaveBeenLastCalledWith(null);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });

  it("does not charge a blocked page to the host: a 40 s page stall around a kill is not a failed close", async () => {
    vi.useFakeTimers();
    try {
      const clock = stallableClock(), pending = deferred<void>(), f = fixture({ kill: () => pending.promise }, { clock });
      await f.owner.start();
      const closing = f.owner.close();
      await later(2000);
      await clock.block(40_000);
      await flush();
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      pending.resolve(); await closing;
      expect(get(f.registry)).toHaveLength(0);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });

  it("counts no time while the window is hidden: a failure nobody can see is not reported", async () => {
    vi.useFakeTimers();
    try {
      const clock = stallableClock(), f = fixture({ kill: () => new Promise<void>(() => {}) }, { clock });
      await f.owner.start();
      const closing = f.owner.close().catch((error: unknown) => error);
      await later(10_000);
      clock.hiddenNow = true;
      await later(300_000);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      clock.hiddenNow = false;
      await later(KILL_HARD_MS - 10_000 - 1000);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      await later(2000);
      expect(await closing).toBeInstanceOf(Error);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });

  it("a restart whose renderer drains at 8 s still starts the replacement", async () => {
    vi.useFakeTimers();
    try {
      const f = fixture();
      await f.owner.start();
      const drained = deferred<void>();
      f.hooks.reset.mockImplementation(() => drained.promise);
      const restarted = f.owner.restart();
      await later(8000);
      expect(f.transport.spawn).toHaveBeenCalledTimes(1);
      drained.resolve(); await restarted;
      expect(f.transport.spawn).toHaveBeenCalledTimes(2);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });

  it("a renderer that never drains fails the restart at its bound, and a hidden window does not count", async () => {
    vi.useFakeTimers();
    try {
      const clock = stallableClock(), f = fixture({}, { clock });
      await f.owner.start();
      f.hooks.reset.mockImplementation(() => new Promise<void>(() => {}));
      const restarted = f.owner.restart();
      await later(RENDERER_DRAIN_MS - 5000);
      clock.hiddenNow = true;
      await later(120_000);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      clock.hiddenNow = false;
      await later(4000);
      expect(f.hooks.state).not.toHaveBeenCalledWith("error", expect.anything());
      await later(2000); await restarted;
      expect(f.hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("Draining terminal renderer timed out"));
      expect(f.transport.spawn).toHaveBeenCalledTimes(1);
      f.owner.dispose(); await flush();
    } finally { vi.useRealTimers(); }
  });
});

describe("a view going away on a slow host", () => {
  it("a detach that answers at 6 s still hands the session to Sessions", async () => {
    vi.useFakeTimers();
    try {
      const { keep, adopted } = keeper(), pending = deferred<boolean>();
      const f = fixture({ detach: () => pending.promise }, { keep });
      await f.owner.start();
      f.owner.dispose();
      await later(6000);
      pending.resolve(true); await flush();
      expect(adopted).toEqual(["native-a"]);
      expect(get(f.registry).map((r) => r.key)).toEqual(["detached:native-a"]);
      expect(f.transport.kill).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });

  it("a detach with no answer keeps ownership and says so, only at its bound", async () => {
    vi.useFakeTimers();
    try {
      const { keep, adopted } = keeper();
      const f = fixture({ detach: () => new Promise<boolean>(() => {}) }, { keep });
      await f.owner.start();
      f.owner.dispose();
      await later(CONTROL_QUICK_MS - 1000);
      expect(get(f.registry)[0].status).not.toContain("could not be detached");
      await later(2000);
      expect(get(f.registry)[0].status).toContain("could not be detached");
      expect(adopted).toEqual([]);
      expect(f.transport.kill).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });

  it("an idle shell whose context answers at 3 s is still ended, not left in Sessions", async () => {
    vi.useFakeTimers();
    try {
      const { keep } = keeper();
      const idle = vi.fn(() => new Promise<boolean>((resolve) => setTimeout(() => resolve(true), 3000)));
      const f = fixture({}, { keep, idle });
      await f.owner.start();
      f.owner.dispose();
      await later(3500);
      expect(f.transport.kill).toHaveBeenCalledWith("native-a");
      expect(f.transport.detach).not.toHaveBeenCalled();
      expect(get(f.registry)).toHaveLength(0);
    } finally { vi.useRealTimers(); }
  });

  it("a context read with no answer is unknown, not idle: the process is kept, at the read bound", async () => {
    vi.useFakeTimers();
    try {
      const { keep } = keeper();
      const f = fixture({}, { keep, idle: () => new Promise<boolean>(() => {}) });
      await f.owner.start();
      f.owner.dispose();
      await later(CONTEXT_READ_MS - 500);
      expect(f.transport.detach).not.toHaveBeenCalled();
      await later(1000);
      expect(f.transport.detach).toHaveBeenCalledWith("native-a");
      expect(f.transport.kill).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });

  it("a session listing that answers at 8 s still lists the session in Sessions", async () => {
    vi.useFakeTimers();
    try {
      const adopted: string[] = [];
      const keep = vi.fn((id: string) => new Promise<(slot: Slot) => void>((resolve) => setTimeout(() => resolve((slot) => {
        slot.handOver({ key: `detached:${id}`, repoPath: "/repo", label: "Shell", status: "Still running", sessionId: id, close: async () => {} });
        adopted.push(id);
      }), 8000)));
      const f = fixture({}, { keep });
      await f.owner.start();
      f.owner.dispose();
      await later(9000);
      expect(adopted).toEqual(["native-a"]);
      expect(KEEP_MS).toBeGreaterThan(8000);
    } finally { vi.useRealTimers(); }
  });

  it("the family lookup alone is bounded and advisory: a lookup stuck behind git leaves the session listed with no family", async () => {
    vi.useFakeTimers();
    try {
      const stuck = vi.fn(() => new Promise<string | null>(() => {}));
      const answer = listingFamily({ run_id: "run-1", repo: "/repo" }, stuck);
      await later(FAMILY_LOOKUP_MS - 500);
      let settled = false;
      void answer.then(() => { settled = true; });
      await flush();
      expect(settled).toBe(false);
      await later(1000);
      expect(await answer).toBeNull();
    } finally { vi.useRealTimers(); }
  });

  it("the family lookup keeps a real answer, skips sessions that are not task attempts, and treats a failed lookup as standing alone", async () => {
    const lookup = vi.fn(async () => "family-key");
    expect(await listingFamily({ run_id: "run-1", repo: "/repo" }, lookup)).toBe("family-key");
    expect(lookup).toHaveBeenCalledWith("/repo");
    lookup.mockClear();
    expect(await listingFamily({ run_id: null, repo: "/repo" }, lookup)).toBeNull();
    expect(lookup).not.toHaveBeenCalled();
    expect(await listingFamily({ run_id: "run-1", repo: "/repo" }, async () => { throw new Error("resolve failed"); })).toBeNull();
  });
});
