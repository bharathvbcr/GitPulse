/**
 * One owner of the PTY event wiring, shared by every terminal tab.
 *
 * `terminal-output` and `terminal-exit` are process-wide events carrying a
 * session id. With one terminal that was a listener with an `if`; with N tabs
 * it would have become N listeners each rejecting N-1 of every chunk — every
 * byte of a `yes` flood decoded once per open tab. This subscribes once and
 * routes by id instead, so cost is O(1) in the number of tabs.
 *
 * It also owns the race that used to live in TerminalPanel: output can arrive
 * between the spawn IPC being sent and its id coming back, so a session cannot
 * subscribe until after its first bytes may already exist. Chunks for an id
 * nobody has claimed are held, bounded, and replayed the moment it subscribes.
 */

import type { TerminalOutputPayload, TerminalExitPayload } from "./runResult";
export type TerminalOutputEvent = TerminalOutputPayload;
export type TerminalExitEvent = TerminalExitPayload;

export interface PtySessionHandlers {
  onOutput: (dataB64: string) => void;
  onExit: (event: TerminalExitEvent) => void;
  onError?: (message: string) => void;
}

/** The subset of Tauri's `listen` this needs, injectable for tests. */
export type EventListen = <T>(
  event: string,
  handler: (event: { payload: T }) => void,
) => Promise<() => void>;

/**
 * Chunks held per unclaimed id, and how many ids may be held at once.
 *
 * Both are eviction bounds, not correctness ones: an id that never subscribes
 * (a session orphaned by a repo switch mid-spawn) would otherwise retain its
 * output for the life of the webview. Oldest id goes first, and within an id
 * the oldest chunk — losing the start of a backlog is better than losing the
 * end, which is where the prompt is.
 */
const MAX_PENDING_CHUNKS_PER_ID = 128;
const MAX_PENDING_IDS = 32;

interface Pending {
  chunks: string[];
  exit: TerminalExitEvent | null;
  incomplete: boolean;
}

export interface PtyBus {
  /** Hold ready listeners across a spawn, including the first session. */
  prepare(): Promise<() => void>;
  /**
   * Routes one session's events until the returned function is called.
   * Anything already buffered for `id` is delivered synchronously first.
   */
  subscribe(id: string, handlers: PtySessionHandlers): () => void;
  /** Held-chunk count for an unclaimed id — for tests and diagnostics. */
  pendingCount(id: string): number;
}

export function createPtyBus(listen: EventListen): PtyBus {
  const handlers = new Map<string, PtySessionHandlers>();
  const pending = new Map<string, Pending>();
  let unlisteners: Array<() => void> = [];
  let preparing = 0;
  let attaching: Promise<void> | null = null;

  const wanted = () => preparing > 0 || handlers.size > 0;

  function report(message: string) {
    for (const handler of handlers.values()) handler.onError?.(message);
  }

  // Tauri subscriptions can fail independently or resolve after a timeout.
  // Each late success releases itself; every partial pair is also unwound.
  function boundedListen<T>(event: string, handler: (event: { payload: T }) => void) {
    return new Promise<() => void>((resolve, reject) => {
      let expired = false;
      const timer = setTimeout(() => {
        expired = true;
        reject(new Error(`Timed out listening for ${event}`));
      }, 5000);
      try {
        void listen<T>(event, handler).then((release) => {
          clearTimeout(timer);
          if (expired) safely(release);
          else resolve(release);
        }, (error: unknown) => { clearTimeout(timer); reject(error); });
      } catch (error) { clearTimeout(timer); reject(error); }
    });
  }

  function pendingFor(id: string): Pending {
    const existing = pending.get(id);
    if (existing) return existing;
    if (pending.size >= MAX_PENDING_IDS) {
      // Map iteration is insertion-ordered: the first key is the oldest id.
      const oldest = pending.keys().next();
      if (!oldest.done) pending.delete(oldest.value);
    }
    const fresh: Pending = { chunks: [], exit: null, incomplete: false };
    pending.set(id, fresh);
    return fresh;
  }

  function handleOutput(payload: TerminalOutputEvent) {
    if (!payload || typeof payload.id !== "string" || typeof payload.data_b64 !== "string") {
      report("Invalid terminal output event");
      return;
    }
    if (payload.data_b64.length > 8192) {
      handlers.get(payload.id)?.onError?.("Terminal output chunk exceeded its limit");
      return;
    }
    const target = handlers.get(payload.id);
    if (target) {
      target.onOutput(payload.data_b64);
      return;
    }
    const held = pendingFor(payload.id);
    held.chunks.push(payload.data_b64);
    if (held.chunks.length > MAX_PENDING_CHUNKS_PER_ID) {
      held.chunks.shift();
      held.incomplete = true;
    }
  }

  function handleExit(payload: TerminalExitEvent) {
    if (!payload || typeof payload.id !== "string" || typeof payload.signal !== "string" ||
        !(payload.exit_code === null || (Number.isInteger(payload.exit_code) && Number.isFinite(payload.exit_code))) ||
        (payload.error != null && typeof payload.error !== "string")) {
      report("Invalid terminal exit event");
      return;
    }
    const target = handlers.get(payload.id);
    if (target) {
      target.onExit(payload);
      return;
    }
    // A session can die before its spawn call returns (a missing agent CLI
    // exits immediately). Holding the exit is what keeps that from reading as
    // a shell that simply never printed anything.
    pendingFor(payload.id).exit = payload;
  }

  function attach(): Promise<void> {
    if (unlisteners.length > 0) return Promise.resolve();
    if (attaching) return attaching;
    const attempt = Promise.allSettled([
      boundedListen<TerminalOutputEvent>("terminal-output", (e) => handleOutput(e.payload)),
      boundedListen<TerminalExitEvent>("terminal-exit", (e) => handleExit(e.payload)),
    ]).then((results) => {
      const releases = results.flatMap((r) => r.status === "fulfilled" ? [r.value] : []);
      const failure = results.find((r) => r.status === "rejected");
      if (failure || !wanted()) {
        for (const release of releases) safely(release);
        if (failure?.status === "rejected") throw failure.reason;
      } else unlisteners = releases;
    }).finally(() => { attaching = null; });
    attaching = attempt;
    return attempt;
  }

  function detach() {
    if (wanted()) return;
    const fns = unlisteners;
    unlisteners = [];
    for (const fn of fns) safely(fn);
    pending.clear();
  }

  return {
    async prepare() {
      preparing += 1;
      let released = false;
      const release = () => {
        if (released) return;
        released = true;
        preparing -= 1;
        detach();
      };
      try { await attach(); return release; }
      catch (error) { release(); throw error; }
    },
    subscribe(id, session) {
      handlers.set(id, session);
      void attach().catch((error: unknown) => session.onError?.(String(error)));
      const held = pending.get(id);
      if (held) {
        pending.delete(id);
        if (held.incomplete) session.onError?.("Terminal output is incomplete: early output exceeded its buffer limit");
        for (const chunk of held.chunks) session.onOutput(chunk);
        if (held.exit) session.onExit(held.exit);
      }
      let released = false;
      return () => {
        if (released) return;
        released = true;
        // Only if still ours: a re-subscribe under the same id (impossible
        // today, cheap to be right about) must not be torn down by the old
        // owner's release.
        if (handlers.get(id) === session) handlers.delete(id);
        detach();
      };
    },
    pendingCount(id) {
      return pending.get(id)?.chunks.length ?? 0;
    },
  };
}

function safely(fn: () => void) {
  try {
    fn();
  } catch {
    /* one dead unlisten must not strand the rest of the unwind */
  }
}
