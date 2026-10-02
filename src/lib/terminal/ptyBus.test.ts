import { describe, expect, it, vi } from "vitest";
import { createSessionLifecycle } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";
import { createPtyBus, MAX_FORGOTTEN_SESSIONS, MAX_PENDING_IDS, MAX_PTY_TOMBSTONES, type EventListen, type TerminalExitEvent } from "./ptyBus";

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

  it("reports an oversized chunk that arrived before subscribe instead of dropping it silently", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    const onError = vi.fn();
    const onOutput = vi.fn();
    transport.emit("terminal-output", output("late", "A".repeat(8193)));
    transport.emit("terminal-output", output("late", "QQ=="));
    bus.subscribe("late", { onOutput, onExit: vi.fn(), onError });
    expect(onOutput).toHaveBeenCalledTimes(1);
    expect(onOutput).toHaveBeenCalledWith("QQ==");
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("exceeded"));
  });

  it("still delivers an exit after that session's early output was discarded", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    transport.emit("terminal-output", output("victim", "QQ=="));
    transport.emit("terminal-exit", exit("victim"));
    for (let i = 0; i < 80; i++) transport.emit("terminal-output", output(`later-${i}`, "QQ=="));
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
    const bus = createPtyBus(transport.listen);
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
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 5000; i++) transport.emit("terminal-output", output(`s-${i % 64}`, "QQ=="));
    for (let i = 0; i < 64; i++) expect(bus.pendingCount(`s-${i}`)).toBeLessThanOrEqual(128);
    const onError = vi.fn();
    const onOutput = vi.fn();
    bus.subscribe("s-0", { onOutput, onExit: vi.fn(), onError });
    expect(onOutput.mock.calls.length).toBeLessThanOrEqual(128);
    expect(onError).toHaveBeenCalled();
  });

  it("reports an oversized chunk to a live session and keeps the following chunk", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const onError = vi.fn();
    const onOutput = vi.fn();
    bus.subscribe("live", { onOutput, onExit: vi.fn(), onError });
    transport.emit("terminal-output", output("live", "A".repeat(8193)));
    transport.emit("terminal-output", output("live", "QQ=="));
    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("exceeded"));
    expect(onOutput).toHaveBeenCalledTimes(1);
    expect(onOutput).toHaveBeenCalledWith("QQ==");
  });

  it("keeps a real exit error when early output was also discarded", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    transport.emit("terminal-output", output("victim", "QQ=="));
    const failed = { ...exit("victim"), reaped: false, exit_code: null, error: "wait failed" };
    transport.emit("terminal-exit", failed);
    for (let i = 0; i < 80; i++) transport.emit("terminal-output", output(`later-${i}`, "QQ=="));
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
    const bus = createPtyBus(transport.listen);
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
    const bus = createPtyBus(transport.listen);
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
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    transport.emit("terminal-output", output("both-facts", "QQ=="));
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
    const bus = createPtyBus(transport.listen);
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
    const bus = createPtyBus(transport.listen);
    const ready = await bus.prepare();
    transport.emit("terminal-output", output("native-a", "QQ=="));
    transport.emit("terminal-exit", exit("native-a"));
    for (let i = 0; i < 80; i++) transport.emit("terminal-output", output(`later-${i}`, "QQ=="));
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
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 128; i++) transport.emit("terminal-output", output("exact", "QQ=="));
    transport.emit("terminal-exit", exit("exact"));
    const exact = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("exact", exact);
    expect(exact.onError).not.toHaveBeenCalled();
    expect(exact.onOutput).toHaveBeenCalledTimes(128);
    expect(exact.onExit).toHaveBeenCalledWith(exit("exact"));

    // Exit first, then the flood: the exit is already stored when the cap trims.
    transport.emit("terminal-exit", exit("trimmed"));
    for (let i = 0; i < 129; i++) transport.emit("terminal-output", output("trimmed", "QQ=="));
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

    transport.emit("terminal-output", output("both", "A".repeat(8193)));
    for (let i = 0; i < 129; i++) transport.emit("terminal-output", output("both", "QQ=="));
    transport.emit("terminal-exit", exit("both"));
    const both = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("both", both);
    const bothExit = both.onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(bothExit.error).toEqual(expect.stringContaining("buffer limit"));
    expect(bothExit.error).toEqual(expect.stringContaining("exceeded"));
    expect(bothExit.exit_code).toBe(0);

    for (let n = 0; n < 200; n++) {
      const id = `burst-${n}`;
      for (let i = 0; i < 129; i++) transport.emit("terminal-output", output(id, "QQ=="));
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
    const bus = createPtyBus(transport.listen);
    const ready = await bus.prepare();
    for (let i = 0; i < 129; i++) transport.emit("terminal-output", output("native-trim", "QQ=="));
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
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    for (let i = 0; i < 129; i++) transport.emit("terminal-output", output("kept", "QQ=="));
    transport.emit("terminal-exit", { ...exit("kept"), reaped: false, exit_code: null, error: "wait failed" });
    const onExit = vi.fn();
    bus.subscribe("kept", { onOutput: vi.fn(), onExit, onError: vi.fn() });
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.error).toContain("buffer limit");
    expect(delivered.exit_code).toBeNull();
  });

  it("keeps a live oversized chunk on the clean exit and still accepts typing until then", async () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const ready = await bus.prepare();
    const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
    const owner = createSessionLifecycle({
      key: "tab",
      repoPath: "/repo",
      label: "Shell",
      registry: createSessionRegistry(),
      transport: {
        spawn: async () => ({ id: "native-live", shell: "/bin/sh", cwd: "/repo" }),
        write: async () => {},
        resize: async () => {},
        kill: async () => {},
      },
      hooks,
      bus,
    });
    await owner.start();
    transport.emit("terminal-output", output("native-live", "A".repeat(8193)));
    transport.emit("terminal-output", output("native-live", "QQ=="));
    expect(owner.write("still-alive")).toBe(true);
    expect(hooks.output).toHaveBeenCalledWith("QQ==", "native-live");
    transport.emit("terminal-exit", exit("native-live"));
    expect(owner.write("after-exit")).toBe(false);
    expect(hooks.exit).toHaveBeenCalledWith(expect.objectContaining({
      exit_code: 0,
      error: expect.stringContaining("exceeded"),
    }));
    expect(hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("exceeded"));
    ready();
    owner.dispose();
  });

  it("keeps an invalid-event notice on the next clean exit of every live session", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const first = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    const second = { onOutput: vi.fn(), onExit: vi.fn(), onError: vi.fn() };
    bus.subscribe("a", first);
    bus.subscribe("b", second);
    transport.emit("terminal-output", null);
    transport.emit("terminal-exit", exit("a"));
    transport.emit("terminal-exit", exit("b"));
    expect(first.onExit.mock.calls[0]?.[0].error).toContain("Invalid");
    expect(second.onExit.mock.calls[0]?.[0].error).toContain("Invalid");
    expect(first.onExit.mock.calls[0]?.[0].exit_code).toBe(0);
  });

  it("replays bytes that arrived after an evicted exit, with both losses", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
    transport.emit("terminal-output", output("mixed", "QQ=="));
    transport.emit("terminal-exit", exit("mixed"));
    for (let i = 0; i < 80; i++) transport.emit("terminal-output", output(`later-${i}`, "QQ=="));
    for (let i = 0; i < 129; i++) transport.emit("terminal-output", output("mixed", "QQ=="));
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
    const bus = createPtyBus(transport.listen);
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

  it("keeps a live oversized chunk when the exit already names its own error", () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const onExit = vi.fn();
    bus.subscribe("live-native", { onOutput: vi.fn(), onExit, onError: vi.fn() });
    transport.emit("terminal-output", output("live-native", "A".repeat(8193)));
    transport.emit("terminal-exit", { ...exit("live-native"), reaped: false, exit_code: null, error: "wait failed" });
    const delivered = onExit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.error).toContain("exceeded");
    expect(delivered.reaped).toBe(false);
    expect(delivered.exit_code).toBeNull();
  });

  it("keeps a live oversized chunk on a native exit error after it reaches the session", async () => {
    const transport = fakeTransport();
    const bus = createPtyBus(transport.listen);
    const ready = await bus.prepare();
    const hooks = { state: vi.fn(), started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(), warning: vi.fn() };
    const owner = createSessionLifecycle({
      key: "tab",
      repoPath: "/repo",
      label: "Shell",
      registry: createSessionRegistry(),
      transport: {
        spawn: async () => ({ id: "native-both", shell: "/bin/sh", cwd: "/repo" }),
        write: async () => {},
        resize: async () => {},
        kill: async () => {},
      },
      hooks,
      bus,
    });
    await owner.start();
    transport.emit("terminal-output", output("native-both", "A".repeat(8193)));
    expect(owner.write("still-alive")).toBe(true);
    transport.emit("terminal-exit", { ...exit("native-both"), reaped: false, exit_code: null, error: "wait failed" });
    expect(owner.write("after-exit")).toBe(false);
    const delivered = hooks.exit.mock.calls[0]?.[0] as TerminalExitEvent;
    expect(delivered.error).toContain("wait failed");
    expect(delivered.error).toContain("exceeded");
    expect(delivered.reaped).toBe(false);
    expect(hooks.state).toHaveBeenLastCalledWith("error", expect.stringContaining("wait failed"));
    expect(hooks.state.mock.calls.at(-1)?.[1]).toContain("exceeded");
    ready();
    owner.dispose();
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

it("forwards a reserved length live and when the chunk is replayed", () => {
  const transport = fakeTransport();
  const bus = createPtyBus(transport.listen);
  // A listener has to be attached before the chunk exists. Emitting first
  // never reaches the bus: nothing is subscribed to the transport yet.
  const anchor = bus.subscribe("anchor", { onOutput: vi.fn(), onExit: vi.fn() });
  transport.emit("terminal-output", { id: "early", data_b64: "QQ==", bytes: 1 });
  const early = { onOutput: vi.fn(), onExit: vi.fn() };
  bus.subscribe("early", early);
  expect(early.onOutput).toHaveBeenCalledTimes(1);
  expect(early.onOutput).toHaveBeenCalledWith("QQ==", 1);
  anchor();

  const live = { onOutput: vi.fn(), onExit: vi.fn() };
  bus.subscribe("live", live);
  transport.emit("terminal-output", { id: "live", data_b64: "Qg==", bytes: 1 });
  expect(live.onOutput).toHaveBeenCalledWith("Qg==", 1);
});

it("does not invent a reserved length for an event that cannot name one", () => {
  const transport = fakeTransport();
  const bus = createPtyBus(transport.listen);
  const live = { onOutput: vi.fn(), onExit: vi.fn() };
  bus.subscribe("live", live);
  for (const bytes of [undefined, 0, 1.5, 4097, -1]) {
    transport.emit("terminal-output", { id: "live", data_b64: "QQ==", bytes });
  }
  expect(live.onOutput).toHaveBeenCalledTimes(5);
  for (const call of live.onOutput.mock.calls) expect(call).toEqual(["QQ=="]);
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
