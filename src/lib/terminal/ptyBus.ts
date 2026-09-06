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

export interface TerminalOutputEvent {
  id: string;
  data_b64: string;
}

export interface TerminalExitEvent {
  id: string;
  exit_code: number | null;
  signal: string;
}

export interface PtySessionHandlers {
  onOutput: (dataB64: string) => void;
  onExit: (event: TerminalExitEvent) => void;
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
}

export interface PtyBus {
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
  /**
   * Bumped on every teardown so a `listen()` promise that resolves afterwards
   * unregisters itself instead of leaving a live listener behind — the same
   * race `createListenerTracker` exists for, inlined here because attachment
   * is refcounted rather than owned by one component.
   */
  let generation = 0;
  /**
   * True between requesting the subscriptions and holding them. Without it,
   * closing the last tab and opening another before the first `listen()`
   * settled started a SECOND subscription beside the still-live first one,
   * and every chunk in that window reached the session twice — duplicated
   * bytes in the terminal, which reads as a corrupted shell rather than a
   * bookkeeping slip.
   */
  let attaching = false;

  function pendingFor(id: string): Pending {
    const existing = pending.get(id);
    if (existing) return existing;
    if (pending.size >= MAX_PENDING_IDS) {
      // Map iteration is insertion-ordered: the first key is the oldest id.
      const oldest = pending.keys().next();
      if (!oldest.done) pending.delete(oldest.value);
    }
    const fresh: Pending = { chunks: [], exit: null };
    pending.set(id, fresh);
    return fresh;
  }

  function handleOutput(payload: TerminalOutputEvent) {
    const target = handlers.get(payload.id);
    if (target) {
      target.onOutput(payload.data_b64);
      return;
    }
    const held = pendingFor(payload.id);
    held.chunks.push(payload.data_b64);
    if (held.chunks.length > MAX_PENDING_CHUNKS_PER_ID) held.chunks.shift();
  }

  function handleExit(payload: TerminalExitEvent) {
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

  function attach() {
    if (attaching || unlisteners.length > 0 || handlers.size === 0) return;
    attaching = true;
    const mine = ++generation;
    void Promise.all([
      listen<TerminalOutputEvent>("terminal-output", (e) => handleOutput(e.payload)),
      listen<TerminalExitEvent>("terminal-exit", (e) => handleExit(e.payload)),
    ]).then((fns) => {
      attaching = false;
      if (mine === generation && handlers.size > 0) {
        unlisteners = fns;
        return;
      }
      // Invalidated while in flight: drop what arrived, then re-evaluate —
      // a tab opened during the unwind still needs a live subscription.
      for (const fn of fns) safely(fn);
      attach();
    });
  }

  function detach() {
    if (handlers.size > 0) return;
    // Invalidates any attach still in flight, so it unregisters itself.
    generation += 1;
    const fns = unlisteners;
    unlisteners = [];
    for (const fn of fns) safely(fn);
    pending.clear();
  }

  return {
    subscribe(id, session) {
      handlers.set(id, session);
      attach();
      const held = pending.get(id);
      if (held) {
        pending.delete(id);
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
