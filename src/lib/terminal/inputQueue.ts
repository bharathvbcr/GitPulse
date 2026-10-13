/** Ordered user input. Never retry a write: it may have been partially accepted. */
const MAX_INPUT_BYTES = 1024 * 1024;
const CHUNK_BYTES = 16 * 1024;
const encoder = new TextEncoder();

/**
 * The page's own sense of time, separate from the timers it arms so a test (and
 * the stall rule below) can tell the two apart.
 */
export interface DeadlineClock {
  now(): number;
  /** Whether the window is hidden, where WebKit throttles or suspends the page. */
  hidden(): boolean;
}

const pageClock: DeadlineClock = {
  now: () => performance.now(),
  hidden: () => typeof document !== "undefined" && document.visibilityState === "hidden",
};

/** How often a deadline looks at the clock. A late tick is how a stalled page shows itself. */
const TICK_MS = 250;
/**
 * A tick later than this over its due time means the page itself was blocked or
 * suspended. Nothing says how much of that gap the backend had the answer
 * pending, so only the tick that was due is credited: a blocked page never
 * turns its own stall into the host's slowness.
 */
const STALL_MS = 500;

/** The deadline passed with no answer. Not an answer: the work may still be in flight. */
export class TerminalTimeoutError extends Error {
  constructor(readonly label: string, readonly ms: number) {
    super(`${label} timed out`);
    this.name = "TerminalTimeoutError";
  }
}

export interface DeadlineOptions {
  /**
   * Time with no answer after which `onSlow(true)` is called, once. It is
   * called with false when the work settles, however it settles, so a notice
   * can be retracted. The work keeps being awaited; this is a side channel.
   */
  softMs?: number;
  onSlow?: (slow: boolean) => void;
  clock?: DeadlineClock;
  /** Count no time while the window is hidden. For waits that must outlive an occluded window. */
  pauseWhileHidden?: boolean;
  /** Asked at each tick; true gives up the wait (the owner is gone) and rejects. */
  cancelled?: () => boolean;
}

/**
 * Bounds how long `work` is awaited by time the page actually spent waiting.
 * Time is counted in short ticks read off `performance.now()` rather than by one
 * long `setTimeout`, so that a page that was stalled (a blocked thread, a
 * suspended window) does not charge the stall to the backend as latency. This
 * is the one place deadlines are measured; every caller gets the same rule.
 */
export function terminalDeadline<T>(work: Promise<T>, ms: number, label: string, options: DeadlineOptions = {}): Promise<T> {
  const { softMs, onSlow, pauseWhileHidden = false, cancelled } = options;
  const clock = options.clock ?? pageClock;
  return new Promise<T>((resolve, reject) => {
    let counted = 0;
    let last = clock.now();
    let slow = false;
    let done = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const finish = () => {
      done = true;
      if (timer !== null) clearTimeout(timer);
      if (slow) { slow = false; onSlow?.(false); }
    };
    const arm = () => {
      const untilSoft = softMs !== undefined && onSlow && !slow ? softMs - counted : Infinity;
      const delay = Math.max(1, Math.min(TICK_MS, ms - counted, untilSoft));
      timer = setTimeout(() => tick(delay), delay);
    };
    const tick = (due: number) => {
      if (done) return;
      const now = clock.now();
      const gap = now - last;
      last = now;
      if (cancelled?.()) { finish(); reject(new Error(`${label} abandoned`)); return; }
      const stalled = gap < 0 || gap > due + STALL_MS;
      if (!(pauseWhileHidden && clock.hidden())) counted += stalled ? due : gap;
      if (counted >= ms) { finish(); reject(new TerminalTimeoutError(label, ms)); return; }
      if (softMs !== undefined && onSlow && !slow && counted >= softMs) { slow = true; onSlow(true); }
      arm();
    };
    void work.then(
      (value) => { if (!done) { finish(); resolve(value); } },
      (error: unknown) => { if (!done) { finish(); reject(error); } },
    );
    arm();
  });
}

/**
 * Words that mark a backend answer as "there is no session any more". Pinned to
 * Rust's `SESSION_GONE_MARKERS` (`terminal/mod.rs`) by `inputQueue.test.ts`,
 * which also holds every answer the write and resize paths can give to this rule.
 */
export const SESSION_GONE_MARKERS = ["not found", "session is closed", "Terminal closed"] as const;
const SESSION_GONE = new RegExp(SESSION_GONE_MARKERS.map((marker) => `\\b${marker}\\b`).join("|"), "i");

/** The backend says there is no session to type into. */
export function sessionIsGone(error: unknown): boolean {
  return SESSION_GONE.test(String(error));
}

/**
 * Whether a failed send ends the tab's input for good. Two things do: the
 * session is gone (nothing left to type into), or the send never answered
 * within the hard bound, so it may still be in flight and a later write could
 * land out of order. A rejected write is different: the backend answered, the
 * outcome is known, and ending the tab over it (a restart kills the process, an
 * agent included) destroys more than the write. A merely slow answer is not
 * here at all: it is awaited.
 */
export function inputIsDead(error: unknown): boolean {
  return error instanceof TerminalTimeoutError || sessionIsGone(error);
}

/** Soft deadline: input is not stopped, the user is told it is queued. */
export const INPUT_SOFT_MS = 5000;
/**
 * Hard deadline: 60 s with no answer at all. The backend answers every write
 * within its own 2 s cap (`terminal/input.rs::write_with_timeout`), on the
 * session's own input lane (`terminal/executor.rs`), which no git command
 * shares and which this queue, sending one chunk at a time, never fills. What
 * is left between the two is the webview's IPC and a host too loaded to
 * schedule either side. That is the trade: long enough to ride out an
 * overloaded host (the case that motivated this), short enough that a send
 * which is never going to answer does not hold the tab for good.
 */
export const INPUT_HARD_MS = 60_000;
export const INPUT_SLOW_NOTICE = "Terminal is responding slowly; typing is queued and will be sent in order.";

export function createTerminalInput(
  send: (data: string, binary: boolean) => Promise<void>,
  onError: (message: string, fatal: boolean) => void,
  options: {
    /** Told once when a send passes the soft deadline, and with null when it settles. */
    onSlow?: (message: string | null) => void;
    clock?: DeadlineClock;
    hardMs?: number;
  } = {},
) {
  const hardMs = options.hardMs ?? INPUT_HARD_MS;
  const queue: Array<{ data: string; binary: boolean }> = [];
  let bytes = 0;
  let stopped = false;
  let pumping: Promise<void> | null = null;

  async function pump() {
    try {
      while (!stopped && queue.length) {
        const chunk = queue.shift()!;
        // The real send is awaited to its end, so what was typed meanwhile
        // stays queued behind it and goes out in order. Slow is told, not fatal.
        await terminalDeadline(send(chunk.data, chunk.binary), hardMs, "Terminal input", {
          clock: options.clock,
          pauseWhileHidden: true,
          cancelled: () => stopped,
          ...(options.onSlow ? { softMs: INPUT_SOFT_MS, onSlow: (slow: boolean) => options.onSlow?.(slow ? INPUT_SLOW_NOTICE : null) } : {}),
        });
        bytes -= chunk.binary ? chunk.data.length : encoder.encode(chunk.data).length;
      }
    } catch (error) {
      if (!stopped) {
        if (inputIsDead(error)) {
          stopped = true;
          onError(
            error instanceof TerminalTimeoutError
              // All that is known is that nothing answered. The session may be fine;
              // the write may still land, so nothing after it can safely be sent.
              ? `Input to this terminal stopped: no answer from the terminal backend for ${Math.round(error.ms / 1000)} s; the host may be overloaded. The session may still be running, but its input cannot be resumed in order; restart to type again.`
              : `Input delivery failed; restart before typing again. ${String(error)}`,
            true,
          );
        } else {
          // The failed chunk may have been partly accepted, so it is never
          // retried, and what was queued behind it was typed after the gap and
          // would land in the wrong place. Both are dropped and said so; the
          // next thing the user types goes through.
          queue.length = 0;
          bytes = 0;
          onError(`Some input was not delivered and was dropped. ${String(error)}`, false);
        }
      }
    } finally {
      if (stopped) { queue.length = 0; bytes = 0; }
      pumping = null;
    }
  }

  return {
    write(data: string, binary = false): boolean {
      if (stopped) return false;
      if (!data) return true;
      const length = binary ? data.length : data.length > MAX_INPUT_BYTES ? data.length : encoder.encode(data).length;
      if (data.length > MAX_INPUT_BYTES || bytes + length > MAX_INPUT_BYTES) {
        onError("Paste was not sent: pending terminal input is limited to 1 MiB.", false);
        return false;
      }
      // Rust strings require Unicode scalar values. With the u flag this
      // matches isolated surrogates only, leaving valid pairs untouched.
      if (!binary && /[\uD800-\uDFFF]/u.test(data)) {
        onError("Invalid Unicode terminal input was not sent.", false);
        return false;
      }
      if (binary && Array.from(data).some((c) => (c.codePointAt(0) ?? 0) > 255)) {
        onError("Invalid binary terminal input was not sent.", false);
        return false;
      }
      const previous = queue.at(-1);
      let chunk = previous?.binary === binary ? queue.pop()!.data : "";
      let size = binary ? chunk.length : encoder.encode(chunk).length;
      // Iteration is by Unicode code point, so a chunk boundary never bisects
      // a surrogate pair. Bracketed-paste markers remain in the same stream.
      for (const character of data) {
        const code = character.codePointAt(0) ?? 0;
        const width = binary || code <= 0x7f ? 1 : code <= 0x7ff ? 2 : code <= 0xffff ? 3 : 4;
        if (size + width > CHUNK_BYTES) { queue.push({ data: chunk, binary }); chunk = ""; size = 0; }
        chunk += character;
        size += width;
      }
      if (chunk) queue.push({ data: chunk, binary });
      bytes += length;
      if (!pumping) pumping = pump();
      return true;
    },
    idle: () => pumping ?? Promise.resolve(),
    dispose() { stopped = true; queue.length = 0; },
  };
}
