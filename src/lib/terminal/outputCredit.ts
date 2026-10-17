/**
 * Frontend half of the PTY output window.
 *
 * Rust `OutputFlow.reserve` counts every emitted chunk until
 * `cmd_terminal_ack` subtracts it. The reader blocks when that debt sits
 * for 30 seconds, then skips output (never the process) until it is repaid. Credit therefore has to be released
 * once for every chunk the view accepted, including a chunk the emulator
 * never paints: the view was already gone, or it was not open yet and the
 * hold reached the window. Releasing early, before an emulator exists, would
 * throw away the prompt. Releasing twice is rejected by the backend.
 *
 * The window size is the Rust constant. `outputCredit.test.ts` reads it.
 */
export const OUTPUT_CREDIT_WINDOW = 512 * 1024;

/**
 * One output message on a session's channel. The reader coalesces what a
 * program wrote in a burst into one message (up to this many bytes) because
 * every message costs the webview a script and a fetch. The credit Rust
 * reserved for a message is exactly its length, so no length travels beside
 * the bytes; one longer than this is not a message the host sends and is
 * repaid without being painted (`planTerminalOutput`). Pinned to Rust's
 * `OUTPUT_CHUNK_BYTES` by `outputCredit.test.ts`.
 */
export const MAX_TERMINAL_OUTPUT_CHUNK = 64 * 1024;

const OVERSIZE = "Terminal output chunk exceeded its limit";

export interface OutputView {
  /** An emulator exists and has not been disposed. */
  terminal: boolean;
  disposed: boolean;
}

export interface PaintToken {
  sessionId: string;
  bytes: Uint8Array;
  /** Bytes to acknowledge. Zero when this chunk was already released. */
  release: () => number;
}

export interface OutputAck {
  sessionId: string;
  bytes: number;
}

export type CreditDecision =
  | { action: "paint"; token: PaintToken }
  | { action: "ack"; sessionId: string; bytes: number }
  | { action: "held" }
  | { action: "overflow"; owed: OutputAck[] };

interface Held {
  sessionId: string;
  bytes: Uint8Array;
}

/**
 * One session view's outstanding output credit.
 *
 * Painted chunks stay outstanding until their release function runs. Held
 * chunks are bytes the emulator could not paint yet; they are not acknowledged
 * until it can, or until the view is given up. `releaseAll` covers both, once.
 */
export function createOutputCredit() {
  let generation = 0;
  let heldBytes = 0;
  const pending = new Map<string, number>();
  const held: Held[] = [];

  function addPending(sessionId: string, bytes: number) {
    pending.set(sessionId, (pending.get(sessionId) ?? 0) + bytes);
  }

  function track(sessionId: string, bytes: number): () => number {
    const gen = generation;
    addPending(sessionId, bytes);
    let done = false;
    return () => {
      if (done || gen !== generation) return 0;
      done = true;
      const left = (pending.get(sessionId) ?? 0) - bytes;
      if (left > 0) pending.set(sessionId, left);
      else pending.delete(sessionId);
      return bytes;
    };
  }

  function snapshot(): OutputAck[] {
    generation += 1;
    const totals = new Map<string, number>(pending);
    pending.clear();
    for (const item of held) {
      totals.set(item.sessionId, (totals.get(item.sessionId) ?? 0) + item.bytes.length);
    }
    held.length = 0;
    heldBytes = 0;
    const owed: OutputAck[] = [];
    for (const [sessionId, bytes] of totals) {
      if (bytes > 0) owed.push({ sessionId, bytes });
    }
    return owed;
  }

  return {
    /** Painted-but-unacked bytes plus bytes still held for a future emulator. */
    pending(): number {
      let total = heldBytes;
      for (const bytes of pending.values()) total += bytes;
      return total;
    },
    accept(bytes: Uint8Array, sessionId: string, view: OutputView): CreditDecision {
      if (bytes.length === 0) return { action: "ack", sessionId, bytes: 0 };
      if (view.disposed) return { action: "ack", sessionId, bytes: bytes.length };
      if (!view.terminal) {
        if (bytes.length > OUTPUT_CREDIT_WINDOW || heldBytes + bytes.length > OUTPUT_CREDIT_WINDOW) {
          const owed = snapshot();
          const row = owed.find((item) => item.sessionId === sessionId);
          if (row) row.bytes += bytes.length;
          else owed.push({ sessionId, bytes: bytes.length });
          return { action: "overflow", owed };
        }
        held.push({ sessionId, bytes });
        heldBytes += bytes.length;
        return { action: "held" };
      }
      return { action: "paint", token: { sessionId, bytes, release: track(sessionId, bytes.length) } };
    },
    /** Hand held bytes to the emulator. Empty when nothing was waiting. */
    flush(): PaintToken[] {
      const items = held.splice(0, held.length);
      heldBytes = 0;
      return items.map((item) => ({
        sessionId: item.sessionId,
        bytes: item.bytes,
        release: track(item.sessionId, item.bytes.length),
      }));
    },
    releaseAll(): OutputAck[] {
      return snapshot();
    },
  };
}

export type OutputPlan =
  | { action: "paint"; token: PaintToken }
  | { action: "ack"; bytes: number; failure: string | null }
  | { action: "held" }
  | { action: "overflow"; owed: OutputAck[]; failure: string };

/**
 * Decide what one output message does to the credit window.
 *
 * Rust reserved exactly `bytes.length` for this message, so that is what is
 * owed whatever becomes of it. A message longer than a read can make is not
 * painted: it is repaid in full and reported. Repaying is what keeps one bad
 * message from holding the reader's window shut until its stall timeout; it
 * was previously dropped with its credit still outstanding.
 */
export function planTerminalOutput(
  credit: ReturnType<typeof createOutputCredit>,
  bytes: Uint8Array,
  sessionId: string,
  view: OutputView,
): OutputPlan {
  if (bytes.length > MAX_TERMINAL_OUTPUT_CHUNK) {
    return { action: "ack", bytes: bytes.length, failure: OVERSIZE };
  }
  const decision = credit.accept(bytes, sessionId, view);
  if (decision.action === "paint") return { action: "paint", token: decision.token };
  if (decision.action === "ack") return { action: "ack", bytes: decision.bytes, failure: null };
  if (decision.action === "held") return { action: "held" };
  return { action: "overflow", owed: decision.owed, failure: "Terminal output was dropped: the view was not open" };
}

/**
 * Owed bytes at which acknowledgements are sent at once rather than at the
 * next frame. A quarter of the window: whatever is being coalesced is never
 * more than this, so the reader always has at least three quarters of
 * `OutputFlow`'s window free and never waits on a frame that has not come.
 */
export const ACK_FLUSH_THRESHOLD = OUTPUT_CREDIT_WINDOW / 4;

/**
 * Upper bound on how long owed credit waits when no frame comes: a hidden
 * window does not run animation frames, and a backgrounded terminal still
 * paints (xterm's write callbacks are not frame-driven).
 */
export const ACK_FLUSH_BACKSTOP_MS = 50;

/** Runs `flush` later; returns a cancel. */
export type AckScheduler = (flush: () => void) => () => void;

/** The next animation frame, or the backstop timer, whichever comes first. */
export const frameAckScheduler: AckScheduler = (flush) => {
  const raf = typeof globalThis.requestAnimationFrame === "function" ? globalThis.requestAnimationFrame : null;
  const frame = raf ? raf(flush) : null;
  const timer = setTimeout(flush, ACK_FLUSH_BACKSTOP_MS);
  return () => {
    if (frame !== null) globalThis.cancelAnimationFrame?.(frame);
    clearTimeout(timer);
  };
};

/**
 * Coalesces `cmd_terminal_ack` calls.
 *
 * Every painted chunk (at most `MAX_TERMINAL_OUTPUT_CHUNK`) used to be
 * acknowledged with its own IPC call. `OutputFlow::acknowledge` accepts any total up to what it
 * has reserved, and the sum of released credit is by construction never more
 * than that, so one call per session per frame carries the same credit.
 * Owed credit is sent at once when any session reaches
 * `ACK_FLUSH_THRESHOLD`, and `flush()` sends everything synchronously — the
 * view's teardown calls it so no credit is left behind with the view.
 */
export function createAckCoalescer(
  send: (sessionId: string, bytes: number) => void,
  schedule: AckScheduler = frameAckScheduler,
) {
  const owed = new Map<string, number>();
  let cancel: (() => void) | null = null;

  function flush() {
    if (cancel) {
      const stop = cancel;
      cancel = null;
      stop();
    }
    if (owed.size === 0) return;
    const rows = [...owed];
    owed.clear();
    for (const [sessionId, bytes] of rows) send(sessionId, bytes);
  }

  return {
    /** Owe `bytes` to `sessionId`. Zero and negative counts are ignored. */
    add(sessionId: string, bytes: number) {
      if (!(bytes > 0)) return;
      const total = (owed.get(sessionId) ?? 0) + bytes;
      owed.set(sessionId, total);
      if (total >= ACK_FLUSH_THRESHOLD) flush();
      else if (!cancel) cancel = schedule(flush);
    },
    /** Send everything owed now. */
    flush,
    /** Bytes owed and not yet sent. */
    owed(): number {
      let total = 0;
      for (const bytes of owed.values()) total += bytes;
      return total;
    },
  };
}
