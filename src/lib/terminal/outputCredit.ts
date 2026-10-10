/**
 * Frontend half of the PTY output window.
 *
 * Rust `OutputFlow.reserve` counts every emitted chunk until
 * `cmd_terminal_ack` subtracts it. The reader blocks, then kills the child,
 * when that debt sits for 30 seconds. Credit therefore has to be released
 * once for every chunk the view accepted, including a chunk the emulator
 * never paints: the view was already gone, or it was not open yet and the
 * hold reached the window. Releasing early, before an emulator exists, would
 * throw away the prompt. Releasing twice is rejected by the backend.
 *
 * The window size is the Rust constant. `outputCredit.test.ts` reads it.
 */
export const OUTPUT_CREDIT_WINDOW = 256 * 1024;

/** One native read. A reserved length outside this range is not a length. */
export const MAX_TERMINAL_OUTPUT_CHUNK = 4096;

/**
 * The byte count the reader reserved, or null when the event cannot name one.
 * Zero, fractions, and anything above one read are refused: acknowledging them
 * would either be rejected or would shrink the window by the wrong amount.
 */
export function reservedTerminalBytes(value: unknown): number | null {
  if (typeof value !== "number" || !Number.isInteger(value)) return null;
  if (value < 1 || value > MAX_TERMINAL_OUTPUT_CHUNK) return null;
  return value;
}

export function decodeTerminalOutput(b64: string): Uint8Array {
  if (typeof b64 !== "string") throw new Error("Terminal output was not base64");
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i) & 0xff;
  return bytes;
}

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
  | { action: "overflow"; owed: OutputAck[]; failure: string }
  | { action: "stop"; failure: string };

/**
 * Decide what one output event does to the credit window.
 *
 * A chunk the emulator cannot decode still occupied `reserved` bytes in
 * `OutputFlow`. Acknowledging that count is what keeps one bad frame from
 * stalling the reader. When the event does not name a usable length, the
 * caller has to stop the session: `flow.stop` is what wakes `reserve`.
 * Guessing the count from the base64 text would acknowledge the wrong number.
 */
export function planTerminalOutput(
  credit: ReturnType<typeof createOutputCredit>,
  b64: string,
  sessionId: string,
  reserved: unknown,
  view: OutputView,
): OutputPlan {
  const reservedBytes = reservedTerminalBytes(reserved);
  let decoded: Uint8Array;
  try {
    decoded = decodeTerminalOutput(b64);
  } catch (err) {
    const failure = `Invalid terminal output: ${err instanceof Error ? err.message : String(err)}`;
    if (reservedBytes !== null) return { action: "ack", bytes: reservedBytes, failure };
    return { action: "stop", failure };
  }
  if (reservedBytes !== null && reservedBytes !== decoded.length) {
    return {
      action: "ack",
      bytes: reservedBytes,
      failure: "Terminal output length did not match its reserved credit",
    };
  }
  const decision = credit.accept(decoded, sessionId, view);
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
export const ACK_FLUSH_THRESHOLD = 64 * 1024;

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
 * Every painted chunk (at most one 4 KiB read) used to be acknowledged with
 * its own IPC call. `OutputFlow::acknowledge` accepts any total up to what it
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
