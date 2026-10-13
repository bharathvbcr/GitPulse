/**
 * One owner of the PTY wiring, shared by every terminal tab.
 *
 * Output arrives on a channel of raw bytes that belongs to one session
 * (`openOutput`, handed to the spawn or attach command), not as a process-wide
 * event every tab would have to reject N-1 of. `terminal-exit` and
 * `terminal-loss` are still process-wide events carrying a session id, and are
 * routed by id from one subscription each, so cost is O(1) in the number of
 * tabs.
 *
 * It also owns the race that used to live in TerminalPanel: output can arrive
 * between the spawn IPC being sent and its id coming back, so a session cannot
 * subscribe until after its first bytes may already exist. A channel holds
 * what arrives before it learns its session's id; bytes for an id nobody has
 * claimed are then held, bounded, and replayed the moment it subscribes.
 *
 * And the order of the end: an exit is an event, and an event overtakes
 * channel messages still being fetched. The host sends a marker down the
 * channel after the last byte, and an exit for a session with a channel waits
 * for it (`EXIT_FENCE_MS` at most).
 */

import type { TerminalExitPayload } from "./runResult";
export type TerminalExitEvent = TerminalExitPayload;

/** What a session's channel delivers: raw bytes, or the marker `"end"` after the last. */
export type OutputMessage = ArrayBuffer | ArrayBufferView | string;
const END_OF_OUTPUT = "end";

/** Longest an exit waits for the end of the session's output before it is delivered anyway. */
export const EXIT_FENCE_MS = 3000;

/** Channel messages held for a session whose id has not come back yet. */
export const MAX_EARLY_MESSAGES = 128;

/** Creates the channel object handed to a command; `onmessage` receives its messages. */
export type ChannelFactory = (onmessage: (message: OutputMessage) => void) => object;

/** One session's output channel, from `PtyBus.openOutput`. */
export interface PtyOutput {
  /** Pass this to the spawn or attach command as `onOutput`. */
  readonly channel: object;
  /** The command returned: this channel is `id`'s. Bytes that came first are delivered. */
  bind(id: string): void;
  /** The command failed: nothing will come, nothing is held. */
  release(): void;
}

export interface PtySessionHandlers {
  /** The bytes of one output message. The credit Rust reserved for it is exactly their count. */
  onOutput: (bytes: Uint8Array) => void;
  onExit: (event: TerminalExitEvent) => void;
  onError?: (message: string) => void;
  /**
   * Output this view was not shown while the process kept running — it fell
   * behind and the host skipped output for it (`terminal-loss`). Not an
   * error: the session is fine, and status must keep saying it runs.
   */
  onNotice?: (message: string) => void;
}

/** `terminal-loss`: the host skipped output for a live session's view. */
export interface TerminalLossEvent {
  id: string;
  message: string;
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
  /** Held, not acknowledged: Rust reserved exactly `bytes.length` for it. */
  bytes: Uint8Array;
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
  /**
   * A channel for one session's output, opened before the command that
   * starts or takes over the session so its first bytes cannot be missed.
   */
  openOutput(): PtyOutput;
}

export function createPtyBus(listen: EventListen, makeChannel: ChannelFactory): PtyBus {
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
    target.onOutput(chunk.bytes);
  }

  function pendingFor(id: string): Pending {
    const existing = pending.get(id);
    if (existing) return existing;
    if (pending.size >= MAX_PENDING_IDS) evictOldestPending();
    const fresh: Pending = { chunks: [], exit: null, incomplete: false, loss: null };
    pending.set(id, fresh);
    return fresh;
  }

  /** One message's bytes for session `id`: to its view, or held until one subscribes. */
  function handleOutput(id: string, bytes: Uint8Array) {
    const target = handlers.get(id);
    if (target) {
      deliverChunk(target, { bytes });
      return;
    }
    const held = pendingFor(id);
    held.chunks.push({ bytes });
    if (held.chunks.length > MAX_PENDING_CHUNKS_PER_ID) {
      held.chunks.shift();
      held.incomplete = true;
    }
  }

  /**
   * Sessions whose channel is bound and has not yet delivered its end marker.
   * Their exit is held until it does: the marker follows the last byte on the
   * same ordered channel, the exit is an event that does not.
   */
  const unfinished = new Set<string>();
  const heldExits = new Map<string, { event: TerminalExitEvent; timer: ReturnType<typeof setTimeout> }>();

  function outputFinished(id: string) {
    unfinished.delete(id);
    const held = heldExits.get(id);
    if (!held) return;
    clearTimeout(held.timer);
    heldExits.delete(id);
    deliverExit(held.event);
  }

  function handleExit(payload: TerminalExitEvent) {
    if (!payload || typeof payload.id !== "string" || typeof payload.signal !== "string" ||
        typeof payload.reaped !== "boolean" || (!payload.reaped && (typeof payload.error !== "string" || !payload.error.trim())) ||
        !(payload.exit_code === null || (Number.isInteger(payload.exit_code) && Number.isFinite(payload.exit_code))) ||
        (payload.error != null && typeof payload.error !== "string")) {
      report("Invalid terminal exit event");
      return;
    }
    const waiting = heldExits.get(payload.id);
    if (waiting) {
      // A second exit must not replace the first; a later error is added to it.
      waiting.event = withNotice(waiting.event, payload.error);
      return;
    }
    if (unfinished.has(payload.id)) {
      // Bytes sent before this exit are still on their way. A bound channel
      // that never delivers its marker (a page that lost it) must not hold
      // the exit for good.
      const id = payload.id;
      heldExits.set(id, { event: payload, timer: setTimeout(() => outputFinished(id), EXIT_FENCE_MS) });
      return;
    }
    deliverExit(payload);
  }

  function deliverExit(payload: TerminalExitEvent) {
    unfinished.delete(payload.id);
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

  function handleLoss(payload: TerminalLossEvent) {
    if (!payload || typeof payload.id !== "string" || typeof payload.message !== "string" || !payload.message.trim()) {
      report("Invalid terminal loss event");
      return;
    }
    // Unclaimed: no view of this session exists to have fallen behind, and
    // whichever view takes it over repaints from the program's next frame.
    handlers.get(payload.id)?.onNotice?.(payload.message.slice(0, 1024));
  }

  function attach(): Promise<void> {
    if (unlisteners.length > 0) return Promise.resolve();
    if (attaching) return attaching;
    const attempt = Promise.allSettled([
      boundedListen<TerminalExitEvent>("terminal-exit", (e) => handleExit(e.payload)),
      boundedListen<TerminalLossEvent>("terminal-loss", (e) => handleLoss(e.payload)),
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
    unfinished.clear();
    for (const held of heldExits.values()) clearTimeout(held.timer);
    heldExits.clear();
  }

  function openOutput(): PtyOutput {
    let id: string | null = null;
    let released = false;
    let ended = false;
    /** What arrived before the session's id did, oldest first, bounded. */
    let early: Uint8Array[] = [];
    let lostEarly = false;
    const channel = makeChannel((message) => {
      if (released && id === null) return;
      if (typeof message === "string") {
        if (message !== END_OF_OUTPUT) return;
        ended = true;
        if (id !== null) outputFinished(id);
        return;
      }
      const bytes = message instanceof ArrayBuffer
        ? new Uint8Array(message)
        : ArrayBuffer.isView(message)
          ? new Uint8Array(message.buffer, message.byteOffset, message.byteLength)
          : null;
      if (!bytes) return;
      if (id !== null) { handleOutput(id, bytes); return; }
      early.push(bytes);
      if (early.length > MAX_EARLY_MESSAGES) { early.shift(); lostEarly = true; }
    });
    return {
      channel,
      bind(session) {
        if (released || id !== null) return;
        id = session;
        if (!ended) unfinished.add(session);
        const held = early;
        early = [];
        for (const bytes of held) handleOutput(session, bytes);
        if (lostEarly) pendingFor(session).incomplete = true;
      },
      release() {
        released = true;
        early = [];
        if (id !== null) unfinished.delete(id);
      },
    };
  }

  return {
    openOutput,
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
