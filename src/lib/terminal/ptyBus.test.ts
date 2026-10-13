import { describe, expect, it, vi } from "vitest";
import { createSessionLifecycle } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";
import {
  createPtyBus, EXIT_FENCE_MS, MAX_EARLY_MESSAGES, MAX_FORGOTTEN_SESSIONS, MAX_PENDING_IDS, MAX_PTY_TOMBSTONES,
  type EventListen, type OutputMessage, type PtyBus, type PtyOutput, type TerminalExitEvent,
} from "./ptyBus";

/** The channel each bus under test was handed, so a test can play the host. */
const channels = new WeakMap<PtyBus, { push: Array<(message: OutputMessage) => void>; bound: Map<string, PtyOutput> }>();

/** A bus whose output channels are plain functions the test calls. */
function testBus(listen: EventListen): PtyBus {
  const state = { push: [] as Array<(message: OutputMessage) => void>, bound: new Map<string, PtyOutput>() };
  const bus = createPtyBus(listen, (onmessage) => {
    state.push.push(onmessage);
    return { onmessage };
  });
  channels.set(bus, state);
  return bus;
}

const encoder = new TextEncoder();

/** The host playing one session's channel: opened and bound on first use, then one message per call. */
function playSession(bus: PtyBus, id: string) {
  const state = channels.get(bus);
  if (!state) throw new Error("not a testBus");
  let output = state.bound.get(id);
  if (!output) {
    output = bus.openOutput();
    const push = state.push[state.push.length - 1];
    state.bound.set(id, output);
    channelOf.set(output, push);
    output.bind(id);
  }
  return channelOf.get(output) as (message: OutputMessage) => void;
}
const channelOf = new WeakMap<PtyOutput, (message: OutputMessage) => void>();

/** The newest channel the bus created: where the host would write for the output just opened. */
function channelPush(bus: PtyBus): (message: OutputMessage) => void {
  const push = channels.get(bus)?.push.at(-1);
  if (!push) throw new Error("no channel was opened");
  return push;
}

/** One output message for session `id`. */
const send = (bus: PtyBus, id: string, text: string) => playSession(bus, id)(encoder.encode(text).buffer as ArrayBuffer);
/** The marker the host sends after a session's last byte. */
const endOutput = (bus: PtyBus, id: string) => playSession(bus, id)("end");
/** The bytes a view is handed for `text`. */
const bytes = (text: string) => encoder.encode(text);

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

const exit = (id: string): TerminalExitEvent => ({ id, exit_code: 0, signal: "", error: null, reaped: true });

describe("ptyBus routing", () => {
  it("delivers each session only its own output", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const a = { onOutput: vi.fn(), onExit: vi.fn() };
    const b = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("term-a", a);
    bus.subscribe("term-b", b);

    send(bus, "term-a", "A");
    send(bus, "term-b", "B");

    expect(a.onOutput).toHaveBeenCalledTimes(1);
    expect(a.onOutput).toHaveBeenCalledWith(bytes("A"));
    expect(b.onOutput).toHaveBeenCalledTimes(1);
    expect(b.onOutput).toHaveBeenCalledWith(bytes("B"));
  });

  it("subscribes to the transport once no matter how many tabs are open", async () => {
    // The reason this exists: N listeners each rejecting N-1 of every chunk
    // meant every byte of a flooding command was decoded once per open tab.
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const releases = Array.from({ length: 8 }, (_, i) =>
      bus.subscribe(`term-${i}`, { onOutput: vi.fn(), onExit: vi.fn() }),
    );
    await settle();
    expect(transport.attached).toBe(2); // output + exit + loss, once each
    for (const release of releases) release();
    await settle();
    expect(transport.attached).toBe(0);
  });

  it("keeps listening while any tab remains", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const first = bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() });
    bus.subscribe("b", { onOutput: vi.fn(), onExit: vi.fn() });
    first();
    await settle();
    expect(transport.attached).toBe(2);
  });

  it("re-attaches after the last tab closes and a new one opens", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() })();
    await settle();
    expect(transport.attached).toBe(0);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("b", handlers);
    await settle();
    expect(transport.attached).toBe(2);
    send(bus, "b", "B");
    expect(handlers.onOutput).toHaveBeenCalledWith(bytes("B"));
  });

  it("never delivers a chunk twice while a close/open straddles attachment", async () => {
    // Closing the last tab and reopening before `listen()` settled used to
    // leave two live subscriptions calling the same router: every byte in
    // that window reached the session twice.
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() })();
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("b", handlers);
    await settle();
    expect(transport.attached).toBe(2);
    send(bus, "b", "B");
    expect(handlers.onOutput).toHaveBeenCalledTimes(1);
  });

  it("stops delivering to a released session", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const a = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("a", a);
    bus.subscribe("keep-alive", { onOutput: vi.fn(), onExit: vi.fn() });
    const release = bus.subscribe("a", a);
    release();
    send(bus, "a", "A");
    expect(a.onOutput).not.toHaveBeenCalled();
  });
});

describe("ptyBus early output", () => {
  it("replays bytes that arrived before the spawn call returned an id", () => {
    // The real race: the shell prints its prompt while cmd_terminal_spawn is
    // still in flight, so nothing can be subscribed yet.
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });

    send(bus, "late", "prompt");
    send(bus, "late", "$ ");
    expect(bus.pendingCount("late")).toBe(2);

    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("late", handlers);
    expect(handlers.onOutput.mock.calls.map(([c]) => new TextDecoder().decode(c))).toEqual(["prompt", "$ "]);
    expect(bus.pendingCount("late")).toBe(0);
  });

  it("replays an exit that beat the spawn response", () => {
    // A missing agent CLI dies immediately; without this the tab shows a
    // shell that simply never printed anything.
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
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
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 500; i++) send(bus, "orphan", `c-${i}`);
    expect(bus.pendingCount("orphan")).toBe(128);

    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("orphan", handlers);
    expect(new TextDecoder().decode(handlers.onOutput.mock.calls.at(-1)?.[0])).toBe("c-499");
  });

  it("evicts the oldest unclaimed id rather than growing without bound", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 40; i++) send(bus, `orphan-${i}`, "x");
    expect(bus.pendingCount("orphan-0")).toBe(0);
    expect(bus.pendingCount("orphan-39")).toBe(1);
  });

  it("still delivers an exit after that session's early output was discarded", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    send(bus, "victim", "A");
    endOutput(bus, "victim");
    transport.emit("terminal-exit", exit("victim"));
    for (let i = 0; i < 80; i++) send(bus, `later-${i}`, "A");
    const onOutput = vi.fn();
    const onError = vi.fn();
    const onExit = vi.fn();
    bus.subscribe("victim", { onOutput, onExit, onError });
    expect(onOutput).not.toHaveBeenCalled();
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("incomplete"));
    expect(onExit).toHaveBeenCalledWith(expect.objectContaining({ id: "victim", exit_code: 0, reaped: true }));
  });

  it("keeps an evicted exit that had no output, without calling it incomplete", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 40; i++) transport.emit("terminal-exit", exit(`gone-${i}`));
    const onError = vi.fn();
    const onExit = vi.fn();
    bus.subscribe("gone-0", { onOutput: vi.fn(), onExit, onError });
    expect(onExit).toHaveBeenCalledWith(expect.objectContaining({ id: "gone-0" }));
    expect(onError).not.toHaveBeenCalled();
  });

  it("stays bounded across a flood of unclaimed sessions", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 5000; i++) send(bus, `s-${i % 64}`, "A");
    for (let i = 0; i < 64; i++) expect(bus.pendingCount(`s-${i}`)).toBeLessThanOrEqual(128);
    const onError = vi.fn();
    const onOutput = vi.fn();
    bus.subscribe("s-0", { onOutput, onExit: vi.fn(), onError });
    expect(onOutput.mock.calls.length).toBeLessThanOrEqual(128);
    expect(onError).toHaveBeenCalled();
  });

  it("keeps a real exit error when early output was also discarded", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    send(bus, "victim", "A");
    const failed = { ...exit("victim"), reaped: false, exit_code: null, error: "wait failed" };
    endOutput(bus, "victim");
    transport.emit("terminal-exit", failed);
    for (let i = 0; i < 80; i++) send(bus, `later-${i}`, "A");
    const onError = vi.fn();
    const onExit = vi.fn();
    bus.subscribe("victim", { onOutput: vi.fn(), onExit, onError });
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("incomplete"));
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.error).toContain("incomplete");
    expect(delivered.reaped).toBe(false);
  });

  it("still reports an exit notice that fell off the tombstone list", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    const remembered = MAX_PENDING_IDS + MAX_PTY_TOMBSTONES;
    for (let i = 0; i < remembered + 1; i++) transport.emit("terminal-exit", exit(`gone-${i}`));
    const dropped = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("gone-0", dropped);
    expect(dropped.onExit).toHaveBeenCalledWith(expect.objectContaining({
      id: "gone-0",
      exit_code: null,
      reaped: false,
      error: expect.stringContaining("discarded"),
    }));
    expect(dropped.onError).toHaveBeenCalledWith(expect.stringContaining("discarded"));
    const recent = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe(`gone-${remembered}`, recent);
    expect(recent.onExit).toHaveBeenCalledWith(expect.objectContaining({ id: `gone-${remembered}` }));
    expect(recent.onError).not.toHaveBeenCalled();
  });

  it("stops naming sessions once the forgotten-id bound is passed", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    const extra = 3;
    const total = MAX_PENDING_IDS + MAX_PTY_TOMBSTONES + MAX_FORGOTTEN_SESSIONS + extra;
    for (let i = 0; i < total; i++) transport.emit("terminal-exit", exit(`gone-${i}`));
    const oldest = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("gone-0", oldest);
    expect(oldest.onExit).not.toHaveBeenCalled();
    expect(oldest.onError).not.toHaveBeenCalled();
    const edge = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe(`gone-${extra}`, edge);
    expect(edge.onExit).toHaveBeenCalledWith(expect.objectContaining({
      id: `gone-${extra}`,
      exit_code: null,
      reaped: false,
      error: expect.stringContaining("discarded"),
    }));
    expect(edge.onError).toHaveBeenCalledWith(expect.stringContaining("discarded"));
    const newest = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe(`gone-${total - 1}`, newest);
    expect(newest.onExit).toHaveBeenCalledWith(expect.objectContaining({ id: `gone-${total - 1}` }));
    expect(newest.onError).not.toHaveBeenCalled();
  });

  it("names both the discarded output and the discarded exit after the id falls off the tombstone list", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    send(bus, "both-facts", "A");
    endOutput(bus, "both-facts");
    transport.emit("terminal-exit", exit("both-facts"));
    // Fill the pending map, then the tombstone list, so this id is forgotten
    // while it is still inside the forgotten-id cap.
    const others = MAX_PENDING_IDS + MAX_PTY_TOMBSTONES;
    for (let i = 0; i < others; i++) transport.emit("terminal-exit", exit(`other-${i}`));
    const onOutput = vi.fn();
    const onExit = vi.fn();
    bus.subscribe("both-facts", { onOutput, onExit, onError: vi.fn() });
    expect(onOutput).not.toHaveBeenCalled();
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.reaped).toBe(false);
    expect(delivered.exit_code).toBeNull();
    expect(delivered.error).toContain("early output was discarded");
    expect(delivered.error).toContain("exit was discarded");
  });

  it("forgets unclaimed exits when the last listener detaches", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const release = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 40; i++) transport.emit("terminal-exit", exit(`gone-${i}`));
    release();
    await settle();
    const onExit = vi.fn();
    const onError = vi.fn();
    bus.subscribe("gone-0", { onOutput: vi.fn(), onExit, onError });
    await settle();
    expect(onExit).not.toHaveBeenCalled();
    expect(onError).not.toHaveBeenCalled();
  });

  it("keeps the discarded-output notice after the exit reaches the session", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const ready = await bus.prepare();
    send(bus, "native-a", "A");
    endOutput(bus, "native-a");
    transport.emit("terminal-exit", exit("native-a"));
    for (let i = 0; i < 80; i++) send(bus, `later-${i}`, "A");
    const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
    const owner = createSessionLifecycle({
      key: "tab",
      repoPath: "/repo",
      label: "Shell",
      registry: createSessionRegistry(),
      transport: {
        spawn: async () => ({ id: "native-a", shell: "/bin/sh", cwd: "/repo" }),
        write: async () => {},
        resize: async () => {},
        kill: async () => {},
        detach: async () => true,
      },
      hooks,
      bus,
    });
    await owner.start();
    ready();
    expect(hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("incomplete"));
    owner.dispose();
  });

  it("keeps a trimmed early buffer on the clean exit that would clear the banner", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 128; i++) send(bus, "exact", "A");
    endOutput(bus, "exact");
    transport.emit("terminal-exit", exit("exact"));
    const exact = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("exact", exact);
    expect(exact.onError).not.toHaveBeenCalled();
    expect(exact.onOutput).toHaveBeenCalledTimes(128);
    expect(exact.onExit).toHaveBeenCalledWith(exit("exact"));

    // Exit first, then the flood: the exit is already stored when the cap trims.
    transport.emit("terminal-exit", exit("trimmed"));
    for (let i = 0; i < 129; i++) send(bus, "trimmed", "A");
    const trimmed = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("trimmed", trimmed);
    expect(trimmed.onOutput).toHaveBeenCalledTimes(128);
    expect(trimmed.onError).toHaveBeenCalledWith(expect.stringContaining("buffer limit"));
    expect(trimmed.onExit).toHaveBeenCalledWith(expect.objectContaining({
      id: "trimmed",
      exit_code: 0,
      reaped: true,
      error: expect.stringContaining("buffer limit"),
    }));

    for (let i = 0; i < 129; i++) send(bus, "both", "A");
    endOutput(bus, "both");
    transport.emit("terminal-exit", exit("both"));
    const both = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("both", both);
    const bothExit = both.onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(bothExit.error).toEqual(expect.stringContaining("buffer limit"));
    expect(bothExit.exit_code).toBe(0);

    for (let n = 0; n < 200; n++) {
      const id = `burst-${n}`;
      for (let i = 0; i < 129; i++) send(bus, id, "A");
      endOutput(bus, id);
      transport.emit("terminal-exit", exit(id));
      const burst = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
      bus.subscribe(id, burst)();
      expect(burst.onOutput).toHaveBeenCalledTimes(128);
      expect(burst.onExit).toHaveBeenCalledWith(expect.objectContaining({
        error: expect.stringContaining("buffer limit"),
      }));
    }
  });

  it("keeps the trimmed-buffer notice after a clean exit reaches the session", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const ready = await bus.prepare();
    for (let i = 0; i < 129; i++) send(bus, "native-trim", "A");
    endOutput(bus, "native-trim");
    transport.emit("terminal-exit", exit("native-trim"));
    const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
    const owner = createSessionLifecycle({
      key: "tab",
      repoPath: "/repo",
      label: "Shell",
      registry: createSessionRegistry(),
      transport: {
        spawn: async () => ({ id: "native-trim", shell: "/bin/sh", cwd: "/repo" }),
        write: async () => {},
        resize: async () => {},
        kill: async () => {},
        detach: async () => true,
      },
      hooks,
      bus,
    });
    await owner.start();
    ready();
    expect(hooks.output).toHaveBeenCalledTimes(128);
    expect(hooks.exit).toHaveBeenCalledWith(expect.objectContaining({
      id: "native-trim",
      exit_code: 0,
      error: expect.stringContaining("buffer limit"),
    }));
    expect(hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("buffer limit"));
    owner.dispose();
  });

  it("keeps a native exit error and the trimmed buffer together", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 129; i++) send(bus, "kept", "A");
    endOutput(bus, "kept");
    transport.emit("terminal-exit", { ...exit("kept"), reaped: false, exit_code: null, error: "wait failed" });
    const onExit = vi.fn();
    bus.subscribe("kept", { onOutput: vi.fn(), onExit, onError: vi.fn() });
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.error).toContain("buffer limit");
    expect(delivered.exit_code).toBeNull();
  });

  it("keeps an invalid-event notice on the next clean exit of every live session", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const first = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    const second = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("a", first);
    bus.subscribe("b", second);
    transport.emit("terminal-loss", null);
    transport.emit("terminal-exit", exit("a"));
    transport.emit("terminal-exit", exit("b"));
    expect(first.onExit.mock.calls[0]?.[0].error).toContain("Invalid");
    expect(second.onExit.mock.calls[0]?.[0].error).toContain("Invalid");
    expect(first.onExit.mock.calls[0]?.[0].exit_code).toBe(0);
  });

  it("replays bytes that arrived after an evicted exit, with both losses", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    send(bus, "mixed", "A");
    endOutput(bus, "mixed");
    transport.emit("terminal-exit", exit("mixed"));
    for (let i = 0; i < 80; i++) send(bus, `later-${i}`, "A");
    for (let i = 0; i < 129; i++) send(bus, "mixed", "A");
    const onOutput = vi.fn();
    const onExit = vi.fn();
    bus.subscribe("mixed", { onOutput, onExit, onError: vi.fn() });
    expect(onOutput).toHaveBeenCalledTimes(128);
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.exit_code).toBe(0);
    expect(delivered.error).toContain("buffer limit");
    expect(delivered.error).toContain("discarded");
  });

  it("keeps the first exit's error when a second clean exit arrives before subscribe", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    transport.emit("terminal-exit", { ...exit("twice"), reaped: false, exit_code: null, error: "wait failed" });
    transport.emit("terminal-exit", exit("twice"));
    const onExit = vi.fn();
    bus.subscribe("twice", { onOutput: vi.fn(), onExit, onError: vi.fn() });
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.reaped).toBe(false);
    expect(delivered.exit_code).toBeNull();
  });

});

describe("ptyBus attachment race", () => {
  it("unregisters a listen() that resolves after the last tab left", async () => {
    // Same race createListenerTracker exists for: without the generation
    // check the listener outlives every subscriber, for the webview lifetime.
    const transport = fakeTransport();
    transport.defer();
    const bus = testBus(transport.listen);
    const release = bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() });
    release();
    transport.flush();
    await settle();
    expect(transport.attached).toBe(0);
  });

  it("still ends up subscribed when a tab opens during that unwind", async () => {
    const transport = fakeTransport();
    transport.defer();
    const bus = testBus(transport.listen);
    bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn() })();
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("b", handlers);
    transport.flush();
    await settle();
    expect(transport.attached).toBe(2);
    send(bus, "b", "B");
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
    const bus = testBus(listen);
    const onError = vi.fn();
    const release = bus.subscribe("a", { onOutput: vi.fn(), onExit: vi.fn(), onError });
    await settle();
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("transport unavailable"));
    // The sibling that did attach (loss) is released.
    expect(unlisten).toHaveBeenCalledTimes(1);
    release();
    failed = false;
    const releaseReady = await bus.prepare();
    releaseReady();
    expect(unlisten).toHaveBeenCalledTimes(3);
  });

  it("listens before the very first spawn, preserving its early output and exit", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const releaseReady = await bus.prepare();
    send(bus, "first", "prompt");
    endOutput(bus, "first");
    transport.emit("terminal-exit", exit("first"));
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    const release = bus.subscribe("first", handlers);
    releaseReady();
    expect(handlers.onOutput).toHaveBeenCalledWith(bytes("prompt"));
    expect(handlers.onExit).toHaveBeenCalledWith(exit("first"));
    release();
    expect(transport.attached).toBe(0);
  });

  it("discloses eviction instead of presenting an incomplete buffer as intact", () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const anchor = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 500; i++) send(bus, "late", "x");
    const onError = vi.fn();
    bus.subscribe("late", { onOutput: vi.fn(), onExit: vi.fn(), onError });
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("incomplete"));
    anchor();
  });
});

it("rejects malformed exit events without breaking other terminal sessions", () => {
  const transport = fakeTransport(), bus = testBus(transport.listen), onError = vi.fn(), onExit = vi.fn();
  bus.subscribe("a", { onOutput: vi.fn(), onExit, onError });
  expect(() => transport.emit("terminal-exit", null)).not.toThrow();
  transport.emit("terminal-exit", { id: "a", exit_code: "success", signal: 7 });
  expect(onExit).not.toHaveBeenCalled();
  expect(onError).toHaveBeenCalledTimes(2);
});

it("refuses unknown reaping evidence and preserves a reported wait failure", () => {
  const transport = fakeTransport(), bus = testBus(transport.listen), onError = vi.fn(), onExit = vi.fn();
  bus.subscribe("a", { onOutput: vi.fn(), onExit, onError });
  transport.emit("terminal-exit", { ...exit("a"), reaped: undefined });
  transport.emit("terminal-exit", { ...exit("a"), reaped: false });
  expect(onExit).not.toHaveBeenCalled();
  expect(onError).toHaveBeenCalledTimes(2);
  const failed = { ...exit("a"), reaped: false, exit_code: null, error: "Could not confirm process exit: wait failed" };
  transport.emit("terminal-exit", failed);
  expect(onExit).toHaveBeenCalledTimes(1);
  const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
  expect(delivered.reaped).toBe(false);
  expect(delivered.exit_code).toBeNull();
  expect(delivered.error).toContain("Could not confirm process exit: wait failed");
  expect(delivered.error).toContain("Invalid terminal exit event");
});

it("bounds listener setup time and releases listeners that arrive after timeout", async () => {
  vi.useFakeTimers();
  let finish: ((release: () => void) => void) | undefined;
  const release = vi.fn();
  const listen: EventListen = (event) => event === "terminal-loss" ? Promise.resolve(release) : new Promise((resolve) => { finish = resolve; });
  const bus = testBus(listen);
  const ready = bus.prepare();
  const rejected = expect(ready).rejects.toThrow("Timed out listening");
  await vi.advanceTimersByTimeAsync(5000); await rejected;
  expect(release).toHaveBeenCalledTimes(1);
  finish?.(release); await vi.advanceTimersByTimeAsync(0);
  expect(release).toHaveBeenCalledTimes(2);
  vi.useRealTimers();
});

describe("ptyBus skipped-output notices", () => {
  it("routes terminal-loss to the session's notice handler only, never as an error or exit", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const a = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn(), onNotice: vi.fn() };
    const b = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn(), onNotice: vi.fn() };
    const releaseA = bus.subscribe("a", a), releaseB = bus.subscribe("b", b);
    await settle();
    transport.emit("terminal-loss", { id: "a", message: "skipped" });
    expect(a.onNotice).toHaveBeenCalledWith("skipped");
    expect(b.onNotice).not.toHaveBeenCalled();
    expect(a.onError).not.toHaveBeenCalled();
    expect(a.onExit).not.toHaveBeenCalled();
    // A later clean exit carries no loss: the session never failed.
    transport.emit("terminal-exit", exit("a"));
    expect(a.onExit).toHaveBeenCalledWith(expect.objectContaining({ error: null }));
    releaseA(); releaseB();
  });

  it("drops a notice for an id nobody holds, and bounds a hostile message", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const a = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn(), onNotice: vi.fn() };
    const release = bus.subscribe("a", a);
    await settle();
    transport.emit("terminal-loss", { id: "nobody", message: "x" });
    expect(bus.pendingCount("nobody")).toBe(0);
    transport.emit("terminal-loss", { id: "a", message: "y".repeat(100_000) });
    expect(a.onNotice.mock.calls[0][0]).toHaveLength(1024);
    for (const bad of [null, {}, { id: "a" }, { id: "a", message: "  " }, { id: 7, message: "m" }]) {
      transport.emit("terminal-loss", bad);
    }
    expect(a.onNotice).toHaveBeenCalledTimes(1);
    expect(a.onError).toHaveBeenCalledWith("Invalid terminal loss event");
    release();
  });
});

describe("ptyBus output channel", () => {
  const text = (call: unknown[]) => new TextDecoder().decode(call[0] as Uint8Array);

  it("hands a session its channel's bytes live, in order", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "one");
    send(bus, "s", "two");
    expect(handlers.onOutput.mock.calls.map(text)).toEqual(["one", "two"]);
  });

  it("holds what a channel carries before its command returns an id, then replays it in order", async () => {
    // The shell prints its prompt while the spawn command is still in flight:
    // the channel exists, the session's id does not.
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const anchor = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    await settle();
    const output = bus.openOutput();
    const push = channelPush(bus);
    push(bytes("prompt"));
    push(bytes("$ "));
    expect(bus.pendingCount("s")).toBe(0);
    output.bind("s");
    expect(bus.pendingCount("s")).toBe(2);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    expect(handlers.onOutput.mock.calls.map(text)).toEqual(["prompt", "$ "]);
    expect(bus.pendingCount("s")).toBe(0);
    anchor();
  });

  it("reads a typed-array view at its own offset and ignores what is neither bytes nor the end marker", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "first");
    const push = channelPush(bus);
    const backing = bytes("xxviewxx");
    push(backing.subarray(2, 6));
    push("noise");
    push(42 as unknown as OutputMessage);
    expect(handlers.onOutput.mock.calls.map(text)).toEqual(["first", "view"]);
  });

  it("bounds what it keeps for a channel whose id never arrives, and says so", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const anchor = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    await settle();
    const output = bus.openOutput();
    const push = channelPush(bus);
    for (let i = 0; i < MAX_EARLY_MESSAGES + 5; i++) push(bytes(`m-${i}`));
    output.bind("s");
    const onError = vi.fn();
    const onOutput = vi.fn();
    bus.subscribe("s", { onOutput, onExit: vi.fn(), onError });
    expect(onOutput).toHaveBeenCalledTimes(MAX_EARLY_MESSAGES);
    expect(text(onOutput.mock.calls.at(-1) ?? [])).toBe(`m-${MAX_EARLY_MESSAGES + 4}`);
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("buffer limit"));
    anchor();
  });

  it("keeps nothing for a channel whose command failed", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const anchor = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    await settle();
    const output = bus.openOutput();
    const push = channelPush(bus);
    push(bytes("before"));
    output.release();
    push(bytes("after"));
    output.bind("s");
    expect(bus.pendingCount("s")).toBe(0);
    anchor();
  });
});

describe("ptyBus exit ordering", () => {
  // An exit is an event; the bytes before it travel on the channel, and the
  // webview can deliver an event ahead of a channel message it is still
  // fetching. The host sends "end" after the last byte; the exit waits for it.
  const exited = (h: { onOutput: ReturnType<typeof vi.fn>; onExit: ReturnType<typeof vi.fn> }) => h.onExit.mock.invocationCallOrder[0];

  it("holds an exit until the channel's end marker, so no byte is delivered after it", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "head");
    transport.emit("terminal-exit", exit("s"));
    expect(handlers.onExit).not.toHaveBeenCalled();
    send(bus, "s", "tail");
    expect(handlers.onExit).not.toHaveBeenCalled();
    endOutput(bus, "s");
    expect(handlers.onExit).toHaveBeenCalledTimes(1);
    expect(handlers.onOutput).toHaveBeenCalledTimes(2);
    expect(Math.max(...handlers.onOutput.mock.invocationCallOrder)).toBeLessThan(exited(handlers) ?? 0);
  });

  it("delivers an exit at once when the end marker has already come", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "all");
    endOutput(bus, "s");
    transport.emit("terminal-exit", exit("s"));
    expect(handlers.onExit).toHaveBeenCalledTimes(1);
  });

  it("does not delay the exit of a session that has no channel", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    transport.emit("terminal-exit", exit("s"));
    expect(handlers.onExit).toHaveBeenCalledTimes(1);
  });

  it("stops waiting for an end marker that never arrives", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "head");
    vi.useFakeTimers();
    try {
      transport.emit("terminal-exit", exit("s"));
      await vi.advanceTimersByTimeAsync(EXIT_FENCE_MS - 1);
      expect(handlers.onExit).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect(handlers.onExit).toHaveBeenCalledTimes(1);
      endOutput(bus, "s");
      expect(handlers.onExit).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the first exit's error when a second exit arrives while the first waits", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "head");
    transport.emit("terminal-exit", { ...exit("s"), reaped: false, exit_code: null, error: "wait failed" });
    transport.emit("terminal-exit", exit("s"));
    endOutput(bus, "s");
    expect(handlers.onExit).toHaveBeenCalledTimes(1);
    const delivered = handlers.onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.reaped).toBe(false);
  });

  it("does not hold the exit of a session whose channel was released", async () => {
    const transport = fakeTransport();
    const bus = testBus(transport.listen);
    const handlers = { onOutput: vi.fn(), onExit: vi.fn() };
    bus.subscribe("s", handlers);
    await settle();
    send(bus, "s", "head");
    channels.get(bus)?.bound.get("s")?.release();
    transport.emit("terminal-exit", exit("s"));
    expect(handlers.onExit).toHaveBeenCalledTimes(1);
  });
});
