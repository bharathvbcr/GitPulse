import { describe, expect, it, vi } from "vitest";
import { createPtyBus, type EventListen, type TerminalExitEvent } from "./ptyBus";

/** A controllable stand-in for Tauri's event transport. */
function fakeTransport() {
  const handlers = new Map<string, Array<(e: { payload: unknown }) => void>>();
  let attached = 0;
  /** Resolves the pending listen() promises on demand, to drive the race. */
  let release: (() => void) | null = null;

  const listen = ((event: string, handler: (e: { payload: never }) => void) => {
    const list = handlers.get(event) ?? [];
    list.push(handler as (e: { payload: unknown }) => void);
    handlers.set(event, list);
    attached += 1;
    const unlisten = () => {
      attached -= 1;
      const current = handlers.get(event) ?? [];
      handlers.set(
        event,
        current.filter((h) => h !== handler),
      );
    };
    if (release) return new Promise<() => void>((resolve) => {
      const prior = release;
      release = () => {
        prior?.();
        resolve(unlisten);
      };
    });
    return Promise.resolve(unlisten);
  }) as EventListen;

  return {
    listen,
    get attached() {
      return attached;
    },
    emit(event: string, payload: unknown) {
      for (const handler of handlers.get(event) ?? []) handler({ payload });
    },
    defer() {
      release = () => {};
    },
    flush() {
      const fn = release;
      release = null;
      fn?.();
    },
  };
}

/** Drains the microtask queue so pending `listen()` promises settle. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

const output = (id: string, data_b64: string) => ({ id, data_b64 });
const exit = (id: string): TerminalExitEvent => ({ id, exit_code: 0, signal: "", error: null, reaped: true });

describe("ptyBus routing", () => {
  it("delivers each session only its own output", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const a = { onOutput: vi.fn(), onExit: vi.fn() };
    const b = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("term-a", a);
    bus.subscribe("term-b", b);

    transport.emit("terminal-output", output("term-a", "QQ=="));
    transport.emit("terminal-output", output("term-b", "Qg=="));

    expect(a.onOutput).toHaveBeenCalledTimes(1);
    expect(a.onOutput).toHaveBeenCalledWith("QQ==");
    expect(b.onOutput).toHaveBeenCalledTimes(1);
    expect(b.onOutput).toHaveBeenCalledWith("Qg==");
  });

  it("subscribes to the transport once no matter how many tabs are open", async () => {
    // The reason this exists: N listeners each rejecting N-1 of every chunk
    // meant every byte of a flooding command was decoded once per open tab.
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const releases = Array.from({ length: 8 }, (_, i) =>
      bus.subscribe(`term-${i}`, { onOutput: vi.fn(), onExit: vi.fn() }),
    );
    await settle();
    expect(transport.attached).toBe(2); // output + exit, once each
    for (const release of releases) release();
    await settle();
    expect(transport.attached).toBe(0);
  });

  it("keeps listening while any tab remains", async () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const first = bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() });
    bus.subscribe("b", { onOutput: vi.fn(), onExit: vi.fn() });
    first();
    await settle();
    expect(transport.attached).toBe(2);
  });

  it("re-attaches after the last tab closes and a new one opens", async () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() })();
    await settle();
    expect(transport.attached).toBe(0);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("b", handlers);
    await settle();
    expect(transport.attached).toBe(2);
    transport.emit("terminal-output", output("b", "Qg=="));
    expect(handlers.onOutput).toHaveBeenCalledWith("Qg==");
  });

  it("never delivers a chunk twice while a close/open straddles attachment", async () => {
    // Closing the last tab and reopening before `listen()` settled used to
    // leave two live subscriptions calling the same router: every byte in
    // that window reached the session twice.
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() })();
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("b", handlers);
    await settle();
    expect(transport.attached).toBe(2);
    transport.emit("terminal-output", output("b", "Qg=="));
    expect(handlers.onOutput).toHaveBeenCalledTimes(1);
  });

  it("stops delivering to a released session", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const a = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("a", a);
    bus.subscribe("keep-alive", { onOutput: vi.fn(), onExit: vi.fn() });
    const release = bus.subscribe("a", a);
    release();
    transport.emit("terminal-output", output("a", "QQ=="));
    expect(a.onOutput).not.toHaveBeenCalled();
  });
});

describe("ptyBus early output", () => {
  it("replays bytes that arrived before the spawn call returned an id", () => {
    // The real race: the shell prints its prompt while cmd_terminal_spawn is
    // still in flight, so nothing can be subscribed yet.
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });

    transport.emit("terminal-output", output("late", "cHJvbXB0"));
    transport.emit("terminal-output", output("late", "JCA="));
    expect(bus.pendingCount("late")).toBe(2);

    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("late", handlers);
    expect(handlers.onOutput.mock.calls.map(([c]) => c)).toEqual(["cHJvbXB0", "JCA="]);
    expect(bus.pendingCount("late")).toBe(0);
  });

  it("replays an exit that beat the spawn response", () => {
    // A missing agent CLI dies immediately; without this the tab shows a
    // shell that simply never printed anything.
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    transport.emit("terminal-exit", exit("late"));

    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("late", handlers);
    expect(handlers.onExit).toHaveBeenCalledWith(exit("late"));
  });

  it("bounds what it holds for an id that never subscribes", () => {
    // A session orphaned mid-spawn would otherwise retain its output for the
    // life of the webview. The tail is kept — that is where the prompt is.
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 500; i++) transport.emit("terminal-output", output("orphan", `c-${i}`));
    expect(bus.pendingCount("orphan")).toBe(128);

    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("orphan", handlers);
    expect(handlers.onOutput.mock.calls.at(-1)?.[0]).toBe("c-499");
  });

  it("evicts the oldest unclaimed id rather than growing without bound", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 40; i++) transport.emit("terminal-output", output(`orphan-${i}`, "x"));
    expect(bus.pendingCount("orphan-0")).toBe(0);
    expect(bus.pendingCount("orphan-39")).toBe(1);
  });
});

describe("ptyBus attachment race", () => {
  it("unregisters a listen() that resolves after the last tab left", async () => {
    // Same race createListenerTracker exists for: without the generation
    // check the listener outlives every subscriber, for the webview lifetime.
    const transport = fakeTransport();
    transport.defer();
    const bus = createPtyBus(transport.listen);
    const release = bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() });
    release();
    transport.flush();
    await settle();
    expect(transport.attached).toBe(0);
  });

  it("still ends up subscribed when a tab opens during that unwind", async () => {
    const transport = fakeTransport();
    transport.defer();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() })();
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("b", handlers);
    transport.flush();
    await settle();
    expect(transport.attached).toBe(2);
    transport.emit("terminal-output", output("b", "Qg=="));
    expect(handlers.onOutput).toHaveBeenCalledTimes(1);
  });
});


describe("ptyBus failure recovery", () => {
  it("releases a successful listener when its sibling fails, then allows retry", async () => {
    const unlisten = vi.fn();
    let failed = true;
    const listen: EventListen = async (event) => {
      if (event === "terminal-exit" && failed) throw new Error("transport unavailable");
      return unlisten;
    };
    const bus = createPtyBus(listen);
    const onError = vi.fn();
    const release = bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn(), onError });
    await settle();
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("transport unavailable"));
    expect(unlisten).toHaveBeenCalledTimes(1);
    release();
    failed = false;
    const releaseReady = await bus.prepare();
    releaseReady();
    expect(unlisten).toHaveBeenCalledTimes(3);
  });

  it("listens before the very first spawn, preserving its early output and exit", async () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const releaseReady = await bus.prepare();
    transport.emit("terminal-output", output("first", "cHJvbXB0"));
    transport.emit("terminal-exit", exit("first"));
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    const release = bus.subscribe("first", handlers);
    releaseReady();
    expect(handlers.onOutput).toHaveBeenCalledWith("cHJvbXB0");
    expect(handlers.onExit).toHaveBeenCalledWith(exit("first"));
    release();
    expect(transport.attached).toBe(0);
  });

  it("discloses eviction instead of presenting an incomplete buffer as intact", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const anchor = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 500; i++) transport.emit("terminal-output", output("late", "eA=="));
    const onError = vi.fn();
    bus.subscribe("late", { onOutput: vi.fn(), onExit: vi.fn(), onError });
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("incomplete"));
    anchor();
  });
});

it("rejects malformed exit events without breaking other terminal sessions", () => {
  const transport = fakeTransport(), bus = createPtyBus(transport.listen), onError = vi.fn(), onExit = vi.fn();
  bus.subscribe("a", { onOutput: vi.fn(), onExit, onError });
  expect(() => transport.emit("terminal-exit", null)).not.toThrow();
  transport.emit("terminal-exit", { id: "a", exit_code: "success", signal: 7 });
  expect(onExit).not.toHaveBeenCalled();
  expect(onError).toHaveBeenCalledTimes(2);
});

it("refuses unknown reaping evidence and preserves a reported wait failure", () => {
  const transport = fakeTransport(), bus = createPtyBus(transport.listen), onError = vi.fn(), onExit = vi.fn();
  bus.subscribe("a", { onOutput: vi.fn(), onExit, onError });
  transport.emit("terminal-exit", { ...exit("a"), reaped: undefined });
  transport.emit("terminal-exit", { ...exit("a"), reaped: false });
  expect(onExit).not.toHaveBeenCalled();
  expect(onError).toHaveBeenCalledTimes(2);
  const failed = { ...exit("a"), reaped: false, exit_code: null, error: "Could not confirm process exit: wait failed" };
  transport.emit("terminal-exit", failed);
  expect(onExit).toHaveBeenCalledWith(failed);
});

it("bounds listener setup time and releases listeners that arrive after timeout", async () => {
  vi.useFakeTimers();
  let finish: ((release: () => void) => void) | undefined;
  const release = vi.fn();
  const listen: EventListen = (event) => event === "terminal-output" ? Promise.resolve(release) : new Promise((resolve) => { finish = resolve; });
  const bus = createPtyBus(listen);
  const ready = bus.prepare();
  const rejected = expect(ready).rejects.toThrow("Timed out listening");
  await vi.advanceTimersByTimeAsync(5000); await rejected;
  expect(release).toHaveBeenCalledTimes(1);
  finish?.(release); await vi.advanceTimersByTimeAsync(0);
  expect(release).toHaveBeenCalledTimes(2);
  vi.useRealTimers();
});
