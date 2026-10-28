/**
 * Writes into xterm that never end inside a DEC 2026 (synchronized output)
 * block.
 *
 * xterm 6.0.0 buffers every row refresh while mode 2026 is on. When the block
 * closes (`CSI ? 2026 l`) it flushes the buffer, but schedules the paint
 * through its requestAnimationFrame render debouncer. `_renderRows` runs when
 * that frame fires and drops the paint if mode 2026 is on again by then
 * (xtermjs/xterm.js#6071). A program that redraws continuously, as Claude
 * Code does, opens the next block as soon as it closes the last, so the
 * frame is lost and the screen only advances when xterm's own 1000 ms safety
 * timeout expires: about one frame a second, blank or stale in between.
 *
 * What decides whether the paint survives is the parser's state when the
 * frame fires, and `WriteBuffer` only yields to the event loop between
 * chunks. So a chunk that ends after a block opened and before it closed
 * leaves the parser inside the block at every yield. GitPulse cuts PTY output
 * into messages wherever the byte count lands (64 KiB, or whatever the read
 * gathered), without regard to frame boundaries, so that is the normal case
 * under load. This module sits where every output path meets xterm and holds
 * back the tail of a chunk from the opening sequence of an unfinished block
 * until the closing sequence arrives, so each write ends outside a block.
 *
 * Only what is needed is tracked: whether a `CSI ? ... 2026 ... h/l` has been
 * seen, which means following CSI framing (7-bit and 8-bit introducers, CAN/SUB
 * aborts). It follows xterm's own transition table (`EscapeSequenceParser.ts`).
 * String payloads (OSC, DCS, APC, PM, SOS) need no state of their own: in
 * xterm's table an ESC or a C1 control from any state, strings included, ends
 * the string and starts what follows, and nothing else in a payload begins a
 * sequence, so a look-alike `[?2026h` inside one is just text here too. xterm's
 * UTF-8 decoder turns C2 80..9F into the 8-bit controls, while a bare
 * 0x80..0x9F byte is not one.
 *
 * Every bound fails open. A block still open after `SYNC_HOLD_DEADLINE_MS`, or
 * holding more than `SYNC_HOLD_MAX_BYTES`, is written as it stands, and the
 * rest of that block passes straight through (the behaviour without this
 * module) until its close. Holding therefore costs at most one deadline of
 * latency, and never withholds credit that the reader needs: see
 * `SYNC_HOLD_MAX_BYTES`.
 *
 * Credit: `push` takes a completion callback for the bytes it was given, and
 * calls it once those bytes (whichever writes they ended up in) have been
 * parsed, or settled without being written (`discard`, a throwing write).
 * Bytes held back are therefore acknowledged when they are written, exactly
 * once, and `outputCredit`'s bookkeeping stays the owner of what is owed.
 */

import { OUTPUT_CREDIT_WINDOW } from "./outputCredit";

/**
 * Longest a block may stay open before what was held is written anyway.
 *
 * Well under xterm's own 1000 ms timeout, which is the delay this exists to
 * avoid, and above any real frame: a frame is one burst of a program's
 * output, a few ms of reader gather plus a webview hop per message. The
 * deadline only has to catch a program that opened a block and stopped (or
 * crashed, or lost a message), where the alternative is a screen that does not
 * move.
 */
export const SYNC_HOLD_DEADLINE_MS = 100;

/**
 * Most bytes held back. A quarter of the credit window, the same figure as
 * `ACK_FLUSH_THRESHOLD`: held bytes are credit the reader has spent and not
 * had back, as are acknowledgements the coalescer is still batching (up to
 * that threshold) and one message in flight (`MAX_TERMINAL_OUTPUT_CHUNK`).
 * 128 KiB + 128 KiB + 64 KiB leaves 192 KiB of the 512 KiB window free, so
 * the reader is never made to wait on a block that has not closed. The cap is
 * enforced in `push`, not by the timer, so it holds with timers throttled in
 * a hidden window.
 */
export const SYNC_HOLD_MAX_BYTES = OUTPUT_CREDIT_WINDOW / 4;

export interface SyncFrameScheduler {
  set(fn: () => void, ms: number): unknown;
  clear(handle: unknown): void;
}

const timers: SyncFrameScheduler = {
  set: (fn, ms) => setTimeout(fn, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export interface SyncFrameWriterOptions {
  /** Hand bytes to the emulator. `written` runs when they have been parsed. May throw. */
  write: (bytes: Uint8Array, written: () => void) => void;
  /** A write started by the deadline timer failed. The held bytes are already settled. */
  onError?: (error: unknown) => void;
  scheduler?: SyncFrameScheduler;
  deadlineMs?: number;
  maxHeldBytes?: number;
}

export interface SyncFrameWriter {
  /**
   * Write `bytes`, or the part of them that ends outside a block; hold the
   * rest. `done` runs once all of `bytes` has been parsed or abandoned. If
   * the emulator's write throws, `done` runs for every byte that failed and
   * the error is rethrown.
   */
  push(bytes: Uint8Array, done: () => void): void;
  /** Write anything held, in order, and let the rest of an open block through unheld. */
  flush(): void;
  /** `flush`, then forget everything the parser has seen: the terminal was reset. */
  reset(): void;
  /** The view is gone: write nothing, settle everything held. Later pushes are settled unwritten. */
  discard(): void;
  /** Bytes currently held back. */
  held(): number;
}

const ESC = 0x1b;
const CAN = 0x18;
const SUB = 0x1a;
/** UTF-8 lead of U+0080..U+00BF; followed by 0x80..0x9F it is a C1 control. */
const C1_LEAD = 0xc2;
/** xterm's `Params` stores 32 parameters and ignores the rest. */
const MAX_PARAMS = 32;
const PARAM_CAP = 1_000_000;
const MODE_SYNC = 2026;

const GROUND = 0;
const ESCAPE = 1;
const CSI = 2;

type Effect = "open" | "close" | null;

/**
 * Follows escape-sequence framing across chunks. `scan` consumes one chunk and
 * reports where each effective DEC 2026 set/reset happened, and where in the
 * chunk the sequence that did it began.
 */
function createScanner() {
  let mode = GROUND;
  /** The previous byte was C2, so a 0x80..0x9F now is a C1 control. */
  let lead = false;
  // The CSI being read.
  let seqStart = 0;
  let priv = false;
  let first = true;
  let bad = false;
  let param = 0;
  let digits = false;
  let sub = false;
  let mainChecked = false;
  let count = 0;
  let sync = false;
  let blockOpen = false;

  function beginCsi(at: number) {
    mode = CSI;
    seqStart = at;
    priv = false;
    first = true;
    bad = false;
    param = 0;
    digits = false;
    sub = false;
    mainChecked = false;
    count = 0;
    sync = false;
  }

  function endMain() {
    if (!mainChecked && digits && param === MODE_SYNC) sync = true;
    mainChecked = true;
  }

  /** An 8-bit control: from any state it begins a sequence, or ends the one in progress. */
  function c1(code: number, at: number) {
    if (code === 0x9b) beginCsi(at);
    else mode = GROUND;
  }

  return {
    open: () => blockOpen,
    reset() {
      mode = GROUND;
      lead = false;
      blockOpen = false;
    },
    /**
     * Consume `bytes`. `onEffect` is told, in order, of each effective set or
     * reset with the index in `bytes` of the byte that began the sequence
     * (clamped to 0 when it began in an earlier chunk).
     */
    scan(bytes: Uint8Array, onEffect: (effect: Exclude<Effect, null>, start: number) => void) {
      const n = bytes.length;
      // A sequence begun in an earlier chunk began before this one.
      seqStart = -1;
      for (let i = 0; i < n; i++) {
        let b = bytes[i];
        let at = i;
        if (mode === GROUND && !lead) {
          // The common case: text. Nothing but ESC and C2 means anything here.
          while (b !== ESC && b !== C1_LEAD && ++i < n) b = bytes[i];
          if (i >= n) return;
          at = i;
        }
        if (lead) {
          lead = false;
          if (b >= 0x80 && b <= 0x9f) {
            c1(b, at - 1 < 0 ? 0 : at - 1);
            continue;
          }
          // Not a C1 pair: the C2 was text (or the start of a multi-byte char); `b` is read on its own.
        }
        if (b === C1_LEAD) {
          lead = true;
          continue;
        }
        if (b === ESC) {
          mode = ESCAPE;
          seqStart = at;
          continue;
        }
        if (b === CAN || b === SUB) {
          mode = GROUND;
          continue;
        }
        switch (mode) {
          case GROUND:
            break;
          case ESCAPE:
            if (b === 0x5b) beginCsi(seqStart);
            else if (b >= 0x20 && b <= 0x2f) {
              // ESC intermediate: the sequence goes on.
            } else if (b < 0x20 || b === 0x7f) {
              // A control executes and the escape goes on.
            } else mode = GROUND;
            break;
          case CSI: {
            if (b < 0x20 || b === 0x7f) break; // executes or is ignored; the sequence goes on
            if (b >= 0x80) {
              // A non-ASCII character: a parse error unless the sequence is already being ignored.
              if (!bad) mode = GROUND;
              break;
            }
            if (b >= 0x30 && b <= 0x39) {
              first = false;
              if (bad || sub || count >= MAX_PARAMS) break;
              digits = true;
              param = Math.min(param * 10 + (b - 0x30), PARAM_CAP);
            } else if (b === 0x3b) {
              first = false;
              if (!bad && count < MAX_PARAMS) {
                endMain();
                count++;
              }
              param = 0;
              digits = false;
              sub = false;
              mainChecked = false;
            } else if (b === 0x3a) {
              first = false;
              if (!bad && !sub) endMain();
              sub = true;
            } else if (b >= 0x3c && b <= 0x3f) {
              if (first && b === 0x3f) priv = true;
              else bad = true;
              first = false;
            } else if (b >= 0x20 && b <= 0x2f) {
              bad = true;
              first = false;
            } else {
              // Final byte (0x40..0x7e).
              mode = GROUND;
              if (!bad && priv && (b === 0x68 || b === 0x6c)) {
                if (count < MAX_PARAMS) endMain();
                if (sync) {
                  const effect = b === 0x68 ? "open" : "close";
                  if (effect === "open" && !blockOpen) {
                    blockOpen = true;
                    onEffect("open", seqStart < 0 ? 0 : seqStart);
                  } else if (effect === "close" && blockOpen) {
                    blockOpen = false;
                    onEffect("close", seqStart < 0 ? 0 : seqStart);
                  }
                }
              }
            }
            break;
          }
        }
      }
    },
  };
}

interface Job {
  remaining: number;
  done: () => void;
  settled: boolean;
}

interface Segment {
  bytes: Uint8Array;
  job: Job;
}

function settle(job: Job) {
  if (job.settled) return;
  if (--job.remaining > 0) return;
  job.settled = true;
  job.done();
}

/** Mark `job` finished at once, whatever is still outstanding for it. */
function abandon(job: Job) {
  if (job.settled) return;
  job.settled = true;
  job.done();
}

function join(parts: Uint8Array[]): Uint8Array {
  if (parts.length === 1) return parts[0];
  let total = 0;
  for (const part of parts) total += part.length;
  const out = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

export function createSyncFrameWriter(options: SyncFrameWriterOptions): SyncFrameWriter {
  const scheduler = options.scheduler ?? timers;
  const deadlineMs = options.deadlineMs ?? SYNC_HOLD_DEADLINE_MS;
  const maxHeld = options.maxHeldBytes ?? SYNC_HOLD_MAX_BYTES;
  const scanner = createScanner();
  let held: Segment[] = [];
  let heldBytes = 0;
  let timer: unknown = null;
  /** A block is open in the parser and was written out unfinished: pass bytes through until it closes. */
  let bypass = false;
  let discarded = false;

  function disarm() {
    if (timer === null) return;
    scheduler.clear(timer);
    timer = null;
  }

  function arm() {
    disarm();
    timer = scheduler.set(() => {
      timer = null;
      try {
        release();
      } catch (error) {
        options.onError?.(error);
      }
    }, deadlineMs);
  }

  /** Write `segments` as one write; every job they belong to settles when it is parsed. */
  function emit(segments: Segment[]) {
    if (segments.length === 0) return;
    const bytes = join(segments.map((s) => s.bytes));
    try {
      options.write(bytes, () => {
        for (const segment of segments) settle(segment.job);
      });
    } catch (error) {
      for (const segment of segments) abandon(segment.job);
      throw error;
    }
  }

  /** Write everything held. A block still open is let through unheld from here to its close. */
  function release() {
    disarm();
    const segments = held;
    held = [];
    heldBytes = 0;
    if (scanner.open()) bypass = true;
    emit(segments);
  }

  return {
    held: () => heldBytes,

    push(bytes, done) {
      if (bytes.length === 0 || discarded) {
        done();
        return;
      }
      const job: Job = { remaining: 0, done, settled: false };
      const wasHeld = held.length > 0;
      // Where, in this chunk, the held tail begins: bytes.length for none,
      // -1 for "everything, including what was already held".
      let retainFrom = scanner.open() && !bypass ? -1 : bytes.length;
      scanner.scan(bytes, (effect, start) => {
        if (effect === "open") {
          bypass = false;
          retainFrom = start;
        } else {
          bypass = false;
          retainFrom = bytes.length;
        }
      });
      if (bypass) retainFrom = bytes.length;

      if (retainFrom === -1) {
        // Still inside the block that was already held: it grows.
        job.remaining = 1;
        held.push({ bytes, job });
        heldBytes += bytes.length;
        if (heldBytes > maxHeld) release();
        return;
      }

      const writePart = bytes.subarray(0, retainFrom);
      const holdPart = bytes.subarray(retainFrom);
      job.remaining = (writePart.length > 0 ? 1 : 0) + (holdPart.length > 0 ? 1 : 0);
      const out = held;
      held = [];
      heldBytes = 0;
      if (wasHeld) disarm();
      if (writePart.length > 0) out.push({ bytes: writePart, job });
      if (holdPart.length > 0) {
        held.push({ bytes: holdPart, job });
        heldBytes = holdPart.length;
        arm();
      }
      try {
        emit(out);
      } finally {
        if (heldBytes > maxHeld) release();
      }
    },

    flush() {
      release();
    },

    reset() {
      try {
        release();
      } finally {
        scanner.reset();
        bypass = false;
      }
    },

    discard() {
      discarded = true;
      disarm();
      const segments = held;
      held = [];
      heldBytes = 0;
      for (const segment of segments) abandon(segment.job);
    },
  };
}
