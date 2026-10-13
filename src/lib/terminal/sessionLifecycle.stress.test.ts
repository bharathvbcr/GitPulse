/**
 * Model-based fuzz of a whole tab's session lifecycle (`createSessionLifecycle`)
 * against a backend that answers in the words the Rust lanes really use, after
 * anything from no time to 70 s or never, while the page's own thread stalls
 * and its window is hidden and shown.
 *
 * `inputQueue.stress.test.ts` fuzzes the input queue alone with a clock handed
 * to it. This drives the lifecycle the view uses, which builds its own queue on
 * the page clock (`performance.now`, `document.visibilityState`), so the page
 * clock is what is perturbed here. It checks, after every operation:
 *
 *  - the Restart path is reached only on evidence: the tab goes to its error
 *    state only after the backend said the session is gone, or after a write
 *    had at least 60 s of counted time with no answer; never for slowness
 *    alone, never for a refusal that leaves the session alive. Nothing here
 *    ever kills the process;
 *  - typed input reaches the backend as the next slice of what was typed, or
 *    after a reported drop the slice that starts where the drop ended;
 *  - a resize never warns for a gone session or for slowness under 60 s, says
 *    a failure once per streak, and the last size asked for is the last one
 *    sent once everything has settled;
 *  - the "responding slowly" notice is shown only while a write is waited on,
 *    and is retracted.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { createSessionLifecycle } from "./sessionLifecycle";
import { createSessionRegistry } from "./sessionRegistry";

const HARD = 60_000;
const TICK = 250;
const STALL_THRESHOLD = 500;
const TOLERANCE = 600;
const SEEDS = [11, 12, 13, 14, 15, 16, 17, 18];
const OPS = 500;

function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** What the backend says when it refuses a write, as `terminal/*.rs` writes it. */
const WRITE_GONE = [
  "Terminal session 'native-a' not found",
  "Terminal closed after writing 3 of 9 input bytes",
  "The terminal input has shut down: the terminal session is closed",
  "terminal_write was dropped before it ran: the terminal session is closed",
];
const WRITE_LIVE = [
  "The terminal input queue is full (64 waiting); try again in a moment",
  "terminal_write panicked: index out of bounds",
  "Could not start a terminal input thread: Resource temporarily unavailable (os error 35)",
  "Terminal input timed out after writing 3 of 6 bytes; input was not retried",
  "Terminal input failed after 0 of 4 bytes: Input/output error (os error 5)",
];
const RESIZE_GONE = ["Terminal session 'native-a' not found"];
const RESIZE_LIVE = [
  "Terminal is busy; the resize gave up after 2000ms",
  "The terminal geometry queue is full (16 waiting); try again in a moment",
  "terminal_resize panicked: boom",
  "Failed to resize terminal: Inappropriate ioctl for device (os error 25)",
];

const PIECES = ["a", "7", " ", "\r", "\x7f", "é", "世", "🚀", "\x1b[A", "ls -la\n"];

interface Pending { minAtStart: number; maxAtStart: number }

interface Stats { goneFatals: number; hardFatals: number; drops: number; slow: number; resizeWarnings: number; latePastMinute: number }

async function simulate(seed: number): Promise<{ violations: string[]; stats: Stats }> {
  const stats: Stats = { goneFatals: 0, hardFatals: 0, drops: 0, slow: 0, resizeWarnings: 0, latePastMinute: 0 };
  const random = rng(seed);
  const pick = <T,>(items: readonly T[]) => items[Math.floor(random() * items.length)];
  const violations: string[] = [];
  const violate = (what: string) => { if (violations.length < 12) violations.push(`seed ${seed}: ${what}`); };

  vi.useFakeTimers();
  let hidden = false;
  vi.stubGlobal("document", { get visibilityState() { return hidden ? "hidden" : "visible"; } });
  const fakeNow = performance.now.bind(performance);
  let stall = 0;
  const nowSpy = vi.spyOn(performance, "now").mockImplementation(() => fakeNow() + stall);
  try {
    // Two-sided bracket of counted time, as in the input queue's fuzz.
    const counted = { min: 0, max: 0, mark: fakeNow() };
    const sync = () => {
      const t = fakeNow();
      if (!hidden) { counted.min += t - counted.mark; counted.max += t - counted.mark; }
      counted.mark = t;
    };

    let typed = "";
    let cursor = 0;
    let dropMark: number | null = null;
    const writes = new Set<Pending>();
    let goneSeen = false;
    let errorState: string | null = null;
    let slowShown = false;

    let lastAsked: { rows: number; cols: number } | null = null;
    let lastSent: { rows: number; cols: number } | null = null;
    const resizes = new Set<Pending>();
    let resizeFailedSinceOk = false;
    let resizeWarnedSinceOk = false;

    const latency = () => {
      const roll = random();
      return roll < 0.5 ? Math.floor(random() * 40)
        : roll < 0.75 ? 1000 + Math.floor(random() * 6000)
        : roll < 0.93 ? 7000 + Math.floor(random() * 50_000)
        : roll < 0.985 ? 57_000 + Math.floor(random() * 13_000)
        : Infinity;
    };
    const answerLater = (ms: number, answer: () => void) => { if (ms === Infinity) return; if (ms === 0) answer(); else setTimeout(answer, ms); };

    const transport = {
      spawn: vi.fn(async () => ({ id: "native-a", shell: "/bin/sh", cwd: "/repo" })),
      kill: vi.fn(async () => { violate("the process was killed"); }),
      detach: vi.fn(async () => true),
      write: vi.fn((_id: string, data: string) => {
        sync();
        if (errorState !== null) violate(`a write was sent after the tab failed: ${JSON.stringify(data.slice(0, 20))}`);
        const start = dropMark ?? cursor;
        if (typed.slice(start, start + data.length) !== data) violate(`send is not the next slice (cursor ${cursor}, dropMark ${dropMark}, got ${JSON.stringify(data.slice(0, 20))})`);
        cursor = start + data.length;
        dropMark = null;
        const entry: Pending = { minAtStart: counted.min, maxAtStart: counted.max };
        writes.add(entry);
        const ms = latency();
        if (ms >= HARD) stats.latePastMinute += 1;
        const outcome = random();
        const refusal = outcome < 0.86 ? null : outcome < 0.9 ? pick(WRITE_GONE) : pick(WRITE_LIVE);
        return new Promise<void>((resolve, reject) => answerLater(ms, () => {
          sync();
          writes.delete(entry);
          if (refusal === null) resolve();
          else { if (WRITE_GONE.includes(refusal)) goneSeen = true; reject(new Error(refusal)); }
        }));
      }),
      resize: vi.fn((_id: string, rows: number, cols: number) => {
        sync();
        lastSent = { rows, cols };
        const entry: Pending = { minAtStart: counted.min, maxAtStart: counted.max };
        resizes.add(entry);
        const ms = Math.min(latency(), 20_000 + Math.floor(random() * 50_000));
        const outcome = random();
        const refusal = outcome < 0.8 ? null : outcome < 0.83 ? pick(RESIZE_GONE) : pick(RESIZE_LIVE);
        return new Promise<void>((resolve, reject) => answerLater(ms, () => {
          sync();
          resizes.delete(entry);
          if (refusal === null) { resizeFailedSinceOk = false; resizeWarnedSinceOk = false; resolve(); }
          else {
            if (!RESIZE_GONE.includes(refusal)) resizeFailedSinceOk = true;
            reject(new Error(refusal));
          }
        }));
      }),
    };
    const oldestPending = (set: Set<Pending>) => [...set].reduce<Pending | null>((a, b) => (a === null || b.minAtStart < a.minAtStart ? b : a), null);

    const hooks = {
      state: (status: string, message?: string) => {
        sync();
        if (status !== "error") return;
        const text = message ?? "";
        if (errorState === null) errorState = text;
        if (/Input delivery failed/.test(text)) {
          stats.goneFatals += 1;
          if (!goneSeen) violate(`a gone-session failure with no gone answer: ${text}`);
        } else if (/no answer from the terminal backend for 60 s/.test(text)) {
          stats.hardFatals += 1;
          // The one send in flight is the oldest unanswered write.
          const pending = oldestPending(writes);
          const most = pending === null ? 0 : counted.max - pending.minAtStart;
          if (most < HARD - TOLERANCE) violate(`hard stop with at most ${Math.round(most)} ms counted: ${text}`);
        } else {
          violate(`the tab failed for something that is not evidence: ${text}`);
        }
      },
      started: vi.fn(), output: vi.fn(), exit: vi.fn(), reset: vi.fn(),
      warning: (message: string) => {
        sync();
        if (/not delivered and was dropped/.test(message)) {
          stats.drops += 1;
          dropMark = typed.length;
          if (WRITE_GONE.some((gone) => message.includes(gone))) violate(`a gone answer was reported as a dropped chunk: ${message}`);
        } else if (/^Resize failed/.test(message)) {
          stats.resizeWarnings += 1;
          if (resizeWarnedSinceOk) violate(`a second resize warning in one failure streak: ${message}`);
          resizeWarnedSinceOk = true;
          if (RESIZE_GONE.some((gone) => message.includes(gone))) violate(`a gone session was reported as a resize failure: ${message}`);
          if (/no answer/.test(message)) {
            const pending = oldestPending(resizes);
            const most = pending === null ? 0 : counted.max - pending.minAtStart;
            if (most < HARD - TOLERANCE) violate(`resize gave up with at most ${Math.round(most)} ms counted`);
          } else if (!resizeFailedSinceOk) {
            violate(`a resize warning with no failure answer: ${message}`);
          }
        } else {
          violate(`unexpected warning: ${message}`);
        }
      },
      slow: (message: string | null) => {
        sync();
        if (message !== null) {
          stats.slow += 1;
          if (slowShown) violate("slow notice shown twice");
          if (writes.size === 0) violate("slow notice with no write being waited on");
          slowShown = true;
        } else {
          slowShown = false;
        }
      },
    };

    const registry = createSessionRegistry();
    const owner = createSessionLifecycle({
      key: `fuzz-${seed}`, repoPath: "/repo", label: "Shell", registry, transport, hooks,
      bus: { prepare: async () => () => {}, pendingCount: () => 0, subscribe: () => () => {} },
    });
    await owner.start();

    const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); };
    const checks = () => {
      sync();
      if (errorState === null) {
        for (const pending of writes) {
          const least = counted.min - pending.maxAtStart;
          if (least >= HARD + TOLERANCE) violate(`no stop after at least ${Math.round(least)} ms counted with a write unanswered`);
        }
      }
      if (slowShown && (writes.size === 0 || errorState !== null)) violate("slow notice still shown with nothing being waited on");
      if (transport.kill.mock.calls.length > 0) violate("kill was called");
    };

    for (let op = 0; op < OPS; op++) {
      const roll = random();
      if (roll < 0.4) {
        let text = ""; const n = 1 + Math.floor(random() * 8);
        for (let i = 0; i < n; i++) text += pick(PIECES);
        if (errorState === null) typed += text;
        const ok = owner.write(text);
        if (ok !== (errorState === null)) violate(`write accepted=${ok} with the tab ${errorState === null ? "running" : "failed"}`);
        if (!ok && errorState === null) typed = typed.slice(0, typed.length - text.length);
      } else if (roll < 0.55) {
        const size = { rows: 10 + Math.floor(random() * 60), cols: 20 + Math.floor(random() * 200) };
        lastAsked = size;
        owner.resize(size.rows, size.cols);
      } else if (roll < 0.83) {
        const r = random();
        const ms = r < 0.5 ? Math.floor(random() * 300) : r < 0.85 ? 300 + Math.floor(random() * 6000) : 6000 + Math.floor(random() * 64_000);
        await vi.advanceTimersByTimeAsync(ms);
      } else if (roll < 0.93) {
        // The page's thread blocks: its clock jumps, no timer fires meanwhile.
        const ms = random() < 0.5 ? Math.floor(random() * STALL_THRESHOLD) : 1000 + Math.floor(random() * 15_000);
        sync();
        stall += ms;
        if (!hidden && ms <= STALL_THRESHOLD) counted.max += ms;
      } else {
        sync();
        hidden = !hidden;
        counted.min -= TICK; counted.max += TICK;
      }
      await flush();
      checks();
    }

    // Everything that can settle settles, in view; then the last size is asked once more.
    sync();
    if (hidden) { hidden = false; counted.min -= TICK; counted.max += TICK; }
    for (let i = 0; i < 12; i++) { await vi.advanceTimersByTimeAsync(20_000); await flush(); checks(); }
    if (errorState === null && lastAsked !== null) {
      const final = lastAsked as { rows: number; cols: number };
      const sent = lastSent as { rows: number; cols: number } | null;
      if (sent === null || sent.rows !== final.rows || sent.cols !== final.cols) {
        violate(`the last size asked for (${final.rows}x${final.cols}) was not the last one sent (${sent ? `${sent.rows}x${sent.cols}` : "none"})`);
      }
    }
    if (errorState === null && dropMark === null && cursor !== typed.length) violate(`typed input was neither sent nor dropped (cursor ${cursor} of ${typed.length})`);
    if (slowShown) violate("a slow notice is still showing after everything settled");
    owner.dispose();
    await flush();
  } finally {
    nowSpy.mockRestore();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  }
  return { violations, stats };
}

describe("a tab's session lifecycle against a slow, silent or refusing backend", () => {
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });
  const total: Stats = { goneFatals: 0, hardFatals: 0, drops: 0, slow: 0, resizeWarnings: 0, latePastMinute: 0 };
  for (const seed of SEEDS) {
    it(`seed ${seed}: ${OPS} operations reach Restart only on evidence and keep order`, async () => {
      const { violations, stats } = await simulate(seed);
      for (const key of Object.keys(total) as Array<keyof Stats>) total[key] += stats[key];
      expect(violations).toEqual([]);
    }, 120_000);
  }
  it("the runs above reached every path the invariants guard", () => {
    expect(total.goneFatals).toBeGreaterThan(0);
    expect(total.hardFatals).toBeGreaterThan(0);
    expect(total.drops).toBeGreaterThan(0);
    expect(total.slow).toBeGreaterThan(0);
    expect(total.resizeWarnings).toBeGreaterThan(0);
    expect(total.latePastMinute).toBeGreaterThan(0);
  });
});
