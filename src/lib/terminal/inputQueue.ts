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

/*
 * The deadlines of the slower terminal commands, in one place. Each is the
 * backend's worst case, read off its code, plus headroom for what the backend
 * cannot see (the webview's IPC, starting a control-pool thread, a host too
 * loaded to schedule either): the same reasoning as `INPUT_HARD_MS`. A bound
 * that is merely near the worst case turns a slow answer into a reported
 * failure, which for a close or a detach is a wrong one: the work is done.
 *
 * Backend worst cases (`src-tauri/src/terminal/mod.rs`):
 *
 *   kill   `kill_session`, on the control pool. Sleeps, which nothing shortens
 *          for a job that ignores the hangup:
 *            HANGUP_GRACE                          2500 ms
 *            the two SIGKILL sweeps after it         40 ms
 *            portable_pty's reap after SIGHUP   4 x 50 = 200 ms
 *            the cleanup poll before "retry Close" 2000 ms
 *                                                  ------
 *                                                  4740 ms (4750 with a poll step)
 *          plus at least four process-table scans on the way (the first
 *          listing, the grace loop's last one, the two sweeps). A scan was
 *          measured at up to 1209 ms on a host at load ~400 (gitpulse.log,
 *          `cmd_terminal_context` work_ms), so 4 x 1.2 s more: about 10 s.
 *          Routine closes logged 2.6 to 3.2 s.
 *   context `session_context`: one scan and two process reads, 1.2 s measured.
 *          It can also wait for the child lock a closing `kill` holds (up to
 *          the kill's own time), but nobody asks for the context of a session
 *          they are closing, and its answer would be moot.
 *   detach, sessions, attach: a session-map lock (a closing kill takes it every
 *          10 ms for an instant) and, for attach, two TIOCSWINSZ ioctls. No
 *          sleep, no scan: well under a second.
 *
 * The frontend bounds, with the ratio over the worst case above:
 *   KILL_HARD_MS      30 s  about 3x the ~10 s kill. A timeout keeps the slot and
 *                           the process id ("Close failed; use Sessions to retry").
 *   KILL_SOFT_MS       6 s  past the 4.75 s a close takes with nothing slow in the
 *                           way, so the notice means "the host", not "the grace".
 *   CONTEXT_READ_MS    5 s  4x the measured read. A user is waiting on the answer
 *                           (a close question, a new tab's directory), so it is
 *                           not stretched further; a timeout is "could not tell".
 *   CONTROL_QUICK_MS  15 s  detach and the session listing: 15x. Nobody waits on
 *                           these, and giving up early costs something real (a
 *                           view that could not be detached; a session left out
 *                           of Sessions), so they are generous.
 *   FAMILY_LOOKUP_MS  10 s  `cmd_resolve_repo` shares the git pool, where git
 *                           commands were logged taking 5 to 160 s. The family is
 *                           advisory (null means "stands alone"), so it must not
 *                           decide whether a session is listed.
 *   RENDERER_DRAIN_MS 30 s  no backend in it: the page parsing what it was sent
 *                           (a 256 KiB window plus xterm's own buffer), which a
 *                           starved or hidden page does slowly.
 *
 * All are measured by `terminalDeadline`, so a stalled page does not count and
 * `pauseWhileHidden` is set where a window nobody is looking at would otherwise
 * report a failure it cannot show: the close and the detach (they change the
 * process's state) and the renderer drain (a hidden page does not drain).
 * `terminalBounds.test.ts` holds the backend figures above to the Rust source.
 */
export const KILL_HARD_MS = 30_000;
export const KILL_SOFT_MS = 6_000;
export const CONTEXT_READ_MS = 5_000;
export const CONTROL_QUICK_MS = 15_000;
export const FAMILY_LOOKUP_MS = 10_000;
/** A session listing, then the optional family lookup: what handing a session to Sessions may take. */
export const KEEP_MS = CONTROL_QUICK_MS + FAMILY_LOOKUP_MS;
export const RENDERER_DRAIN_MS = 30_000;
export const CLOSE_SLOW_NOTICE = "Closing is taking longer than usual; the terminal host is responding slowly.";

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
