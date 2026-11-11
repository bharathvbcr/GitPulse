import { OUTPUT_CREDIT_WINDOW } from "./outputCredit";

/**
 * Maximum bytes coalesced into a single animation-frame `term.write(...)` call.
 *
 * Sized to `OUTPUT_CREDIT_WINDOW / 4` (128 KiB), matching `SYNC_HOLD_MAX_BYTES`
 * and `ACK_FLUSH_THRESHOLD` so the sum of held sync bytes, queued frame bytes,
 * coalesced acks, and an in-flight chunk stays inside the 512 KiB PTY credit
 * window without stalling the reader.
 */
export const FRAME_WRITE_BUDGET_BYTES = OUTPUT_CREDIT_WINDOW / 4;

/**
 * Backstop delay (ms) when `requestAnimationFrame` is paused (hidden or
 * minimized window). Ensures `written` callbacks still run promptly so PTY
 * output credit is returned to the backend.
 */
export const FRAME_WRITE_BACKSTOP_MS = 16;

export interface RafWriteQueueOptions {
  write: (bytes: Uint8Array, written: () => void) => void;
  onError?: (error: unknown) => void;
  budgetBytes?: number;
  backstopMs?: number;
  raf?: (callback: FrameRequestCallback) => number;
  caf?: (handle: number) => void;
  setTimer?: (run: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
}

export interface RafWriteQueue {
  /** Queue a chunk already framed outside DEC ?2026 blocks. */
  enqueue(bytes: Uint8Array, written: () => void): void;
  /** Synchronously hand all queued bytes to `write` in order (used before reset/exit). */
  flushSync(): void;
  /** Cancel pending timers and settle all queued callbacks without writing (teardown). */
  discard(): void;
  /** Bytes currently waiting for the next frame. */
  queuedBytes(): number;
}

interface QueuedChunk {
  bytes: Uint8Array;
  written: () => void;
  settled: boolean;
}

function settleChunk(chunk: QueuedChunk) {
  if (chunk.settled) return;
  chunk.settled = true;
  chunk.written();
}

function concatChunks(chunks: readonly QueuedChunk[]): Uint8Array {
  if (chunks.length === 1) return chunks[0].bytes;
  let total = 0;
  for (const chunk of chunks) total += chunk.bytes.length;
  const merged = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk.bytes, offset);
    offset += chunk.bytes.length;
  }
  return merged;
}

/**
 * Frame-budgeted write coalescer sitting between `syncFrames` and `term.write`.
 *
 * `syncFrames` guarantees every chunk handed here ends *outside* a DEC `?2026`
 * synchronized-output block. Coalescing **whole** chunks per animation frame
 * collapses 20+ IPC bursts during heavy agent output into a single `term.write`
 * without ever splitting a chunk back open inside `?2026`.
 */
export function createRafWriteQueue(options: RafWriteQueueOptions): RafWriteQueue {
  const budgetBytes = options.budgetBytes ?? FRAME_WRITE_BUDGET_BYTES;
  const backstopMs = options.backstopMs ?? FRAME_WRITE_BACKSTOP_MS;
  const raf =
    options.raf ??
    (typeof globalThis.requestAnimationFrame === "function"
      ? (cb: FrameRequestCallback) => globalThis.requestAnimationFrame(cb)
      : null);
  const caf =
    options.caf ??
    (typeof globalThis.cancelAnimationFrame === "function"
      ? (handle: number) => globalThis.cancelAnimationFrame(handle)
      : null);
  const setTimer = options.setTimer ?? ((fn: () => void, ms: number) => setTimeout(fn, ms));
  const clearTimer =
    options.clearTimer ?? ((handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>));

  const pending: QueuedChunk[] = [];
  let totalBytes = 0;
  let frameHandle: number | null = null;
  let timerHandle: unknown = null;
  let discarded = false;

  function cancelScheduled() {
    if (frameHandle !== null) {
      caf?.(frameHandle);
      frameHandle = null;
    }
    if (timerHandle !== null) {
      clearTimer(timerHandle);
      timerHandle = null;
    }
  }

  function writeBatch(batch: QueuedChunk[]) {
    if (batch.length === 0) return;
    const merged = concatChunks(batch);
    try {
      options.write(merged, () => {
        for (const item of batch) settleChunk(item);
      });
    } catch (error) {
      for (const item of batch) settleChunk(item);
      options.onError?.(error);
    }
  }

  function schedule() {
    if (discarded || pending.length === 0) return;
    if (frameHandle !== null || timerHandle !== null) return;
    if (raf) {
      frameHandle = raf(() => {
        frameHandle = null;
        if (timerHandle !== null) {
          clearTimer(timerHandle);
          timerHandle = null;
        }
        drainOneFrame();
      });
    }
    timerHandle = setTimer(() => {
      timerHandle = null;
      if (frameHandle !== null) {
        caf?.(frameHandle);
        frameHandle = null;
      }
      drainOneFrame();
    }, backstopMs);
  }

  function drainOneFrame() {
    if (discarded || pending.length === 0) return;
    const batch: QueuedChunk[] = [];
    let batchBytes = 0;
    while (pending.length > 0) {
      const head = pending[0];
      if (batch.length > 0 && batchBytes + head.bytes.length > budgetBytes) break;
      pending.shift();
      totalBytes -= head.bytes.length;
      batch.push(head);
      batchBytes += head.bytes.length;
    }
    writeBatch(batch);
    if (pending.length > 0) schedule();
  }

  return {
    enqueue(bytes, written) {
      if (discarded || bytes.length === 0) {
        written();
        return;
      }
      pending.push({ bytes, written, settled: false });
      totalBytes += bytes.length;
      schedule();
    },
    flushSync() {
      cancelScheduled();
      if (discarded || pending.length === 0) return;
      const batch = pending.splice(0, pending.length);
      totalBytes = 0;
      writeBatch(batch);
    },
    discard() {
      discarded = true;
      cancelScheduled();
      const rest = pending.splice(0, pending.length);
      totalBytes = 0;
      for (const item of rest) settleChunk(item);
    },
    queuedBytes: () => totalBytes,
  };
}
