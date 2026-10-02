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

import { reservedTerminalBytes } from "./outputCredit";
import type { TerminalOutputPayload, TerminalExitPayload } from "./runResult";
export type TerminalOutputEvent = TerminalOutputPayload;
export type TerminalExitEvent = TerminalExitPayload;

export interface PtySessionHandlers {
  /** `reservedBytes` is omitted when the event did not name a usable length. */
  onOutput: (dataB64: string, reservedBytes?: number) => void;
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
export const MAX_PENDING_CHUNKS_PER_ID = 128;
export const MAX_PENDING_IDS = 32;
/** Exits and loss notices kept after an unclaimed id is evicted from `pending`. */
export const MAX_PTY_TOMBSTONES = MAX_PENDING_IDS * 4;
/** Ids whose tombstone was itself evicted. Subscribe still hears that it was dropped. */
export const MAX_FORGOTTEN_SESSIONS = MAX_PENDING_IDS * 32;

const CHUNK_LIMIT = "Terminal output is incomplete: early output exceeded its buffer limit";
const DISCARDED = "Terminal output is incomplete: early output was discarded before the session was ready";
const OVERSIZE = "Terminal output chunk exceeded its limit";
const FORGOTTEN_EXIT = "Terminal exit was discarded before the session was ready";

/** Keep every distinct loss. A later exit must not replace an earlier one. */
export function mergeTerminalNotice(current: string | null, extra: string | null | undefined): string | null {
  if (!extra) return current;
  if (!current) return extra;
  if (current.includes(extra)) return current;
  if (extra.includes(current)) return extra;
  return `${current} ${extra}`;
}

function withNotice(event: TerminalExitEvent, notice: string | null): TerminalExitEvent {
  if (!notice) return event;
  const error = mergeTerminalNotice(event.error, notice);
  return error === event.error ? event : { ...event, error };
}

interface HeldChunk {
  data: string;
  /** Null when the event did not carry a length the renderer can acknowledge. */
  bytes: number | null;
}

interface Pending {
  chunks: HeldChunk[];
  exit: TerminalExitEvent | null;
  incomplete: boolean;
  /** Set when a chunk was refused, or when this id was previously evicted with output. */
  loss: string | null;
}

interface Tombstone {
  exit: TerminalExitEvent | null;
  loss: string | null;
}

interface Forgotten {
  loss: string | null;
  hadExit: boolean;
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
  const tombstones = new Map<string, Tombstone>();
  const forgotten = new Map<string, Forgotten>();
  /** Losses already shown to a subscribed session. The exit has to carry them. */
  const liveLoss = new Map<string, string>();
  let unlisteners: Array<() => void> = [];
  let preparing = 0;
  let attaching: Promise<void> | null = null;

  const wanted = () => preparing > 0 || handlers.size > 0;

  function noteLive(id: string, message: string) {
    liveLoss.set(id, mergeTerminalNotice(liveLoss.get(id) ?? null, message) ?? message);
  }

  function report(message: string) {
    for (const [id, handler] of handlers) {
      noteLive(id, message);
      handler.onError?.(message);
    }
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

  function forget(id: string, stone: Tombstone) {
    if (!stone.loss && !stone.exit) return;
    if (forgotten.has(id)) forgotten.delete(id);
    while (forgotten.size >= MAX_FORGOTTEN_SESSIONS) {
      const oldest = forgotten.keys().next();
      if (oldest.done) break;
      forgotten.delete(oldest.value);
    }
    forgotten.set(id, { loss: stone.loss, hadExit: stone.exit !== null });
  }

  /** An evicted id keeps its exit and, when output was dropped, a loss notice. */
  function rememberEviction(id: string, entry: Pending) {
    const previous = tombstones.get(id);
    const hadOutput = entry.chunks.length > 0 || entry.incomplete || entry.loss !== null;
    const loss = hadOutput ? (entry.loss ?? previous?.loss ?? DISCARDED) : (previous?.loss ?? null);
    const exit = entry.exit ?? previous?.exit ?? null;
    if (previous) tombstones.delete(id);
    forgotten.delete(id);
    if (!exit && !loss) return;
    while (tombstones.size >= MAX_PTY_TOMBSTONES) {
      const oldest = tombstones.keys().next();
      if (oldest.done) break;
      const droppedId = oldest.value;
      const dropped = tombstones.get(droppedId);
      tombstones.delete(droppedId);
      if (dropped) forget(droppedId, dropped);
    }
    tombstones.set(id, { exit, loss });
  }

  function evictOldestPending() {
    const oldest = pending.keys().next();
    if (oldest.done) return;
    const entry = pending.get(oldest.value);
    pending.delete(oldest.value);
    if (entry) rememberEviction(oldest.value, entry);
  }

  function deliverChunk(target: PtySessionHandlers, chunk: HeldChunk) {
    if (chunk.bytes === null) target.onOutput(chunk.data);
    else target.onOutput(chunk.data, chunk.bytes);
  }

  function pendingFor(id: string): Pending {
    const existing = pending.get(id);
    if (existing) return existing;
    if (pending.size >= MAX_PENDING_IDS) evictOldestPending();
    const fresh: Pending = { chunks: [], exit: null, incomplete: false, loss: null };
    pending.set(id, fresh);
    return fresh;
  }

  function handleOutput(payload: TerminalOutputEvent) {
    if (!payload || typeof payload.id !== "string" || typeof payload.data_b64 !== "string") {
      report("Invalid terminal output event");
      return;
    }
    if (payload.data_b64.length > 8192) {
      const target = handlers.get(payload.id);
      if (target) {
        noteLive(payload.id, OVERSIZE);
        target.onError?.(OVERSIZE);
        return;
      }
      const held = pendingFor(payload.id);
      held.loss = mergeTerminalNotice(held.loss, OVERSIZE);
      return;
    }
    const chunk = { data: payload.data_b64, bytes: reservedTerminalBytes(payload.bytes) };
    const target = handlers.get(payload.id);
    if (target) {
      deliverChunk(target, chunk);
      return;
    }
    const held = pendingFor(payload.id);
    held.chunks.push(chunk);
    if (held.chunks.length > MAX_PENDING_CHUNKS_PER_ID) {
      held.chunks.shift();
      held.incomplete = true;
    }
  }

  function handleExit(payload: TerminalExitEvent) {
    if (!payload || typeof payload.id !== "string" || typeof payload.signal !== "string" ||
        typeof payload.reaped !== "boolean" || (!payload.reaped && (typeof payload.error !== "string" || !payload.error.trim())) ||
        !(payload.exit_code === null || (Number.isInteger(payload.exit_code) && Number.isFinite(payload.exit_code))) ||
        (payload.error != null && typeof payload.error !== "string")) {
      report("Invalid terminal exit event");
      return;
    }
    const target = handlers.get(payload.id);
    if (target) {
      // Every loss already shown has to ride on the exit. A clean exit would
      // otherwise clear the banner, and a native error would otherwise replace
      // it. withNotice appends; it does not replace the process status.
      const notice = liveLoss.get(payload.id) ?? null;
      liveLoss.delete(payload.id);
      target.onExit(withNotice(payload, notice));
      return;
    }
    // A session can die before its spawn call returns (a missing agent CLI
    // exits immediately). Holding the exit is what keeps that from reading as
    // a shell that simply never printed anything. A second exit must not
    // replace the first; a later error is added to it.
    const held = pendingFor(payload.id);
    held.exit = held.exit ? withNotice(held.exit, payload.error) : payload;
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
    tombstones.clear();
    forgotten.clear();
    liveLoss.clear();
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
      liveLoss.delete(id);
      handlers.set(id, session);
      void attach().catch((error: unknown) => session.onError?.(String(error)));
      const held = pending.get(id) ?? null;
      const stone = tombstones.get(id) ?? null;
      const forgot = forgotten.get(id) ?? null;
      pending.delete(id);
      tombstones.delete(id);
      forgotten.delete(id);
      const named = held?.loss ?? stone?.loss ?? forgot?.loss ?? null;
      const loss = named ?? (forgot?.hadExit ? FORGOTTEN_EXIT : null);
      // A clean exit clears the session error banner, so every loss rides on
      // the exit. A trimmed buffer is not itself `loss` until something else
      // refuses a chunk, and a forgotten exit no longer has a payload to replay.
      const parts: string[] = [];
      if (held?.incomplete) parts.push(CHUNK_LIMIT);
      if (named && !parts.includes(named)) parts.push(named);
      // The tombstone holds the exit from before this id was evicted. A newer
      // pending exit adds its error and must not erase the earlier one.
      let exitEvent = stone?.exit ?? null;
      if (held?.exit) exitEvent = exitEvent ? withNotice(exitEvent, held.exit.error) : held.exit;
      let notice = parts.join(" ");
      if (!exitEvent && forgot?.hadExit && !notice.includes(FORGOTTEN_EXIT)) {
        notice = notice ? `${notice} ${FORGOTTEN_EXIT}` : FORGOTTEN_EXIT;
      }
      if (held?.incomplete) session.onError?.(CHUNK_LIMIT);
      if (loss) session.onError?.(loss);
      if (held) for (const chunk of held.chunks) deliverChunk(session, chunk);
      if (exitEvent) {
        session.onExit(withNotice(exitEvent, notice));
      } else if (forgot?.hadExit) {
        session.onExit({
          id,
          exit_code: null,
          signal: "",
          error: notice || FORGOTTEN_EXIT,
          reaped: false,
        });
      }
      let released = false;
      return () => {
        if (released) return;
        released = true;
        // Only if still ours: a re-subscribe under the same id (impossible
        // today, cheap to be right about) must not be torn down by the old
        // owner's release.
        if (handlers.get(id) === session) {
          handlers.delete(id);
          liveLoss.delete(id);
        }
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
