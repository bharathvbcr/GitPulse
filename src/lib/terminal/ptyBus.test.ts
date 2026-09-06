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
const exit = (id: string): TerminalExitEvent => ({ id, exit_code: 0, signal: "" });

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
