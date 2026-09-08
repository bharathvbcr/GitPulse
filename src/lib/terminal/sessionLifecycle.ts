import type { PtyBus, TerminalExitEvent } from "./ptyBus";
import type { TerminalSpawned } from "./runResult";
import { createTerminalInput, terminalDeadline } from "./inputQueue";
import type { createSessionRegistry } from "./sessionRegistry";

export interface SessionTransport {
  spawn(): Promise<TerminalSpawned>;
  write(id: string, data: string, binary: boolean): Promise<void>;
  resize(id: string, rows: number, cols: number): Promise<void>;
  kill(id: string): Promise<void>;
}
export interface SessionHooks {
  state(status: "starting" | "running" | "exited" | "error", message?: string): void;
  started(session: TerminalSpawned): void;
  output(data: string, id: string): void;
  exit(event: TerminalExitEvent): void;
  reset(): void | Promise<void>;
  warning(message: string): void;
}

/** One canonical owner of a tab's native process, even after its view is gone. */
export function createSessionLifecycle(options: {
  key: string;
  repoPath: string;
  label: string;
  bus: PtyBus;
  registry: ReturnType<typeof createSessionRegistry>;
  transport: SessionTransport;
  hooks: SessionHooks;
}) {
  const { bus, registry, transport, hooks } = options;
  let id: string | null = null;
  let disposed = false;
  let starting: Promise<void> | null = null;
  let restarting: Promise<void> | null = null;
  let closing: Promise<void> | null = null;
  let subscription: (() => void) | null = null;
  let slot: ReturnType<typeof registry.reserve> | null = null;
  let input: ReturnType<typeof createTerminalInput> | null = null;
  let pendingSize: { rows: number; cols: number } | null = null;
  let resizing = false;

  function state(status: "starting" | "running" | "exited" | "error", message?: string) {
    slot?.update(message ?? status);
    if (!disposed) hooks.state(status, message);
  }
  function release() {
    input?.dispose(); input = null;
    subscription?.(); subscription = null;
    slot?.release(); slot = null;
    id = null;
  }
  function fail(message: string) {
    input?.dispose();
    state("error", message);
  }
  async function closeCurrent() {
    if (closing) return closing;
    const target = id;
    if (!target) { release(); return; }
    input?.dispose();
    slot?.update("closing");
    closing = terminalDeadline(transport.kill(target), 5000, "Closing terminal").then(() => {
      if (id === target) {
        release();
        if (!disposed) hooks.state("exited");
      }
    }).catch((error: unknown) => {
      // Keep the slot and process id: a failed kill is not a closed process.
      state("error", `Close failed; use Sessions to retry. ${String(error)}`);
      throw error;
    }).finally(() => { closing = null; });
    return closing;
  }
  async function close() {
    if (starting) await starting;
    await closeCurrent();
  }
  async function launch() {
    if (disposed || id) return;
    let ready: (() => void) | null = null;
    let watchdog: ReturnType<typeof setTimeout> | null = null;
    let timedOut = false;
    try {
      slot = registry.reserve({ key: options.key, repoPath: options.repoPath, label: options.label, status: "starting", close });
      state("starting");
      ready = await bus.prepare();
      if (disposed) { release(); return; }
      // A timeout is visible, but an unresolved spawn still owns its slot. A
      // retry must not spawn a duplicate, and a late success must be reclaimed.
      watchdog = setTimeout(() => { timedOut = true; state("error", "Starting terminal timed out; awaiting native cleanup"); }, 15000);
      const spawned = await transport.spawn();
      clearTimeout(watchdog); watchdog = null;
      id = spawned.id;
      if (disposed || timedOut) {
        await closeCurrent();
        if (timedOut) state("error", "Starting terminal timed out; the late process was closed. Retry to start again.");
        return;
      }
      input = createTerminalInput((data, binary) => transport.write(spawned.id, data, binary), (message, fatal) => {
        if (fatal) fail(message);
        else if (!disposed) hooks.warning(message);
      });
      state("running");
      hooks.started(spawned);
      subscription = bus.subscribe(spawned.id, {
        onOutput(data) { if (!disposed && id === spawned.id) hooks.output(data, spawned.id); },
        onError(message) { if (id === spawned.id) fail(message); },
        onExit(event) {
          if (id !== spawned.id) return;
          release();
          if (!disposed) { hooks.exit(event); hooks.state(event.error ? "error" : "exited", event.error ?? undefined); }
        },
      });
      // An exit may have been replayed synchronously by subscribe().
      if (!id) { subscription(); subscription = null; }
      else void resizeLatest();
    } catch (error) {
      state("error", String(error));
      if (!id) release();
    } finally {
      if (watchdog !== null) clearTimeout(watchdog);
      ready?.();
    }
  }
  function start(): Promise<void> {
    if (starting) return starting;
    if (disposed || id) return Promise.resolve();
    starting = launch().finally(() => { starting = null; });
    return starting;
  }
  async function resizeLatest() {
    if (resizing) return;
    resizing = true;
    try {
      while (pendingSize && id && !disposed) {
        const size = pendingSize, target = id;
        pendingSize = null;
        try { await terminalDeadline(transport.resize(target, size.rows, size.cols), 5000, "Terminal resize"); }
        catch (error) { if (!disposed && id === target) hooks.warning(`Resize failed: ${String(error)}`); }
      }
    } finally { resizing = false; }
  }
  return {
    start,
    restart(): Promise<void> {
      if (restarting) return restarting;
      restarting = (async () => {
        if (starting) await starting;
        if (disposed) return;
        await closeCurrent();
        await terminalDeadline(Promise.resolve(hooks.reset()), 5000, "Draining terminal renderer");
        await start();
      })().catch((error: unknown) => state("error", String(error))).finally(() => { restarting = null; });
      return restarting;
    },
    write: (data: string, binary = false) => input?.write(data, binary) ?? false,
    resize(rows: number, cols: number) {
      if (!Number.isFinite(rows) || !Number.isFinite(cols) || rows < 1 || cols < 1) return;
      pendingSize = { rows: Math.min(1000, Math.round(rows)), cols: Math.min(1000, Math.round(cols)) };
      void resizeLatest();
    },
    fail,
    isCurrent: (sessionId: string) => !disposed && id === sessionId,
    dispose() {
      disposed = true;
      input?.dispose();
      pendingSize = null;
      // Failed closes stay in the global registry, with an explicit retry.
      void close().catch(() => { /* closeCurrent retained and reported ownership */ });
    },
  };
}
