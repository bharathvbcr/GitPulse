/**
 * Model-based fuzz of the terminal input queue against a host that answers
 * slowly, never, or with errors, and a page whose own thread stalls.
 *
 * The model does not copy the implementation. It keeps four things of its own:
 * the stream of bytes the queue accepted, a cursor of what has been sent, a
 * two-sided estimate of how much *counted* time (wall time minus page stalls
 * and hidden time) the send in flight has had, and the pending-byte total.
 * From those it checks, after every operation and at every callback:
 *
 *  - delivery: every send is exactly the next slice of the accepted stream, or,
 *    after a reported drop, exactly the slice that starts where the drop ended.
 *    So nothing is reordered, duplicated or retried, and nothing is skipped
 *    silently;
 *  - nothing is sent once the queue has reported a fatal stop;
 *  - the 1 MiB pending bound holds, and a write is refused only for want of room;
 *  - a fatal stop is only ever on evidence: the backend said the session is
 *    gone, or the send in flight had 60 s of counted time with no answer;
 *  - and the other direction: a send with that much counted time and no answer
 *    is fatal, and one with 5 s of it has a visible "responding slowly" notice.
 *
 * Counted time is bracketed, not computed, because a page stall shorter than
 * the stall threshold is rightly counted when a tick observes it and rightly
 * ignored when the answer arrives first. The checks use the bracket's far side
 * for "justified" and its near side for "owed".
 *
 * `INPUT_QUEUE_MODULE` points the same run at another copy of the module, which
 * is how the pre-fix queue was run against it (see the commit message).
 */
import { afterEach, describe, expect, it, vi } from "vitest";

const MODULE = process.env.INPUT_QUEUE_MODULE ?? "./inputQueue";
const { createTerminalInput } = (await import(/* @vite-ignore */ MODULE)) as typeof import("./inputQueue");

const MAX_PENDING = 1024 * 1024;
const SOFT = 5000;
const HARD = 60_000;
/** A stall at or below this is read as latency by a tick that sees it; above it, not. */
const STALL_THRESHOLD = 500;
const TICK = 250;
/** Rounding and ordering: ticks are re-armed at 250 ms, answers land between them. */
const TOLERANCE = 600;
const SEEDS = [1, 2, 3, 4, 5, 6];
const OPS = 600;

function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const PIECES = ["a", "Z", "7", " ", "\r", "\x7f", "é", "世", "🚀", "𝒳", "\x1b[200~", "\x1b[201~", "\x1b[A", "ls -la\n"];
const utf8 = new TextEncoder();

interface InFlight { minAtStart: number; maxAtStart: number; settled: boolean }

interface Stats { slowNotices: number; hardFatals: number; goneFatals: number; drops: number; refusedForRoom: number }

async function simulate(seed: number): Promise<{ violations: string[]; stats: Stats }> {
  const stats: Stats = { slowNotices: 0, hardFatals: 0, goneFatals: 0, drops: 0, refusedForRoom: 0 };
  const random = rng(seed);
  const pick = <T,>(items: readonly T[]) => items[Math.floor(random() * items.length)];
  const violations: string[] = [];
  const violate = (what: string) => { if (violations.length < 12) violations.push(`seed ${seed}: ${what}`); };

  vi.useFakeTimers();
  try {
    // The page's clock: fake time plus whatever the page lost to stalls.
    let stall = 0;
    let hidden = false;
    const clock = { now: () => performance.now() + stall, hidden: () => hidden };

    // The two-sided estimate of counted time.
    const counted = { min: 0, max: 0, mark: performance.now() };
    const sync = () => {
      const t = performance.now();
      if (!hidden) { counted.min += t - counted.mark; counted.max += t - counted.mark; }
      counted.mark = t;
    };

    let accepted = "";
    let acceptedBytes = 0, okBytes = 0, droppedBytes = 0;
    let cursor = 0;
    let dropMark: number | null = null;
    let inflight: InFlight | null = null;
    let goneSeen = false;
    let fatal: string | null = null;
    let fatalCount = 0;
    let slowActive = false;

    const send = (data: string): Promise<void> => {
      sync();
      if (fatal !== null) violate(`send after fatal stop: ${JSON.stringify(data.slice(0, 20))}`);
      const start = dropMark ?? cursor;
      if (accepted.slice(start, start + data.length) !== data) {
        violate(`send is not the next slice of the stream (cursor ${cursor}, dropMark ${dropMark}, len ${data.length}, got ${JSON.stringify(data.slice(0, 20))})`);
      }
      cursor = start + data.length;
      dropMark = null;
      const entry: InFlight = { minAtStart: counted.min, maxAtStart: counted.max, settled: false };
      inflight = entry;
      // How the host answers this send.
      const roll = random();
      const latency = roll < 0.5 ? Math.floor(random() * 40)
        : roll < 0.72 ? 1000 + Math.floor(random() * 5000)
        : roll < 0.9 ? 6000 + Math.floor(random() * 49_000)
        : roll < 0.96 ? 55_000 + Math.floor(random() * 15_000)
        : Infinity;
      const outcome = random();
      const rejection = outcome < 0.85 ? null
        : outcome < 0.92 ? "Terminal input timed out after writing 3 of 6 bytes"
        : outcome < 0.97 ? "Terminal session 'native-a' not found"
        : "pty write failed";
      return new Promise<void>((resolve, reject) => {
        if (latency === Infinity) return;
        const answer = () => {
          sync();
          entry.settled = true;
          if (inflight === entry) inflight = null;
          if (rejection === null) { okBytes += utf8.encode(data).length; resolve(); }
          else {
            if (/not found/.test(rejection)) goneSeen = true;
            reject(new Error(rejection));
          }
        };
        if (latency === 0) answer();
        else setTimeout(answer, latency);
      });
    };

    const queue = createTerminalInput(
      (data: string, binary: boolean) => {
        if (binary) violate("binary send in a text-only run");
        return send(data);
      },
      (message: string, isFatal: boolean) => {
        sync();
        if (isFatal) {
          fatalCount += 1;
          if (fatalCount > 1) violate("a second fatal stop");
          fatal = message;
          if (goneSeen && /not found/.test(message)) { stats.goneFatals += 1; return; }
          const current = inflight as InFlight | null;
          if (current === null) { violate(`fatal stop with nothing in flight and no gone answer: ${message}`); return; }
          stats.hardFatals += 1;
          const mostItCouldHave = counted.max - current.minAtStart;
          if (mostItCouldHave < HARD - TOLERANCE) {
            violate(`fatal stop with at most ${Math.round(mostItCouldHave)} ms counted: ${message}`);
          }
          if (!/no answer from the terminal backend for 60 s/.test(message)) violate(`fatal message does not say what is known: ${message}`);
        } else if (/not delivered/.test(message)) {
          // Everything accepted so far is gone: the chunk that failed and what was behind it.
          stats.drops += 1;
          dropMark = accepted.length;
          droppedBytes = acceptedBytes - okBytes;
        }
      },
      {
        clock,
        onSlow: (message: string | null) => {
          sync();
          if (message !== null) {
            stats.slowNotices += 1;
            if (slowActive) violate("slow notice shown twice without a retraction");
            slowActive = true;
            const current = inflight as InFlight | null;
            if (current === null) violate("slow notice with nothing in flight");
            else if (counted.max - current.minAtStart < SOFT - TOLERANCE) violate(`slow notice after at most ${Math.round(counted.max - current.minAtStart)} ms counted`);
          } else {
            if (!slowActive) violate("retraction of a notice that was not shown");
            slowActive = false;
          }
        },
      },
    );

    const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); };
    const randomText = () => {
      const roll = random();
      if (roll < 0.9) { let s = ""; const n = 1 + Math.floor(random() * 12); for (let i = 0; i < n; i++) s += pick(PIECES); return s; }
      if (roll < 0.95) { let s = ""; const n = 1500 + Math.floor(random() * 9000); for (let i = 0; i < n; i++) s += pick(PIECES); return s; }
      // A paste big enough to fill the bound when the host is not answering.
      const unit = pick(PIECES.slice(6, 10)).repeat(40) + "\n";
      return "\x1b[200~" + unit.repeat(Math.floor((300_000 + random() * 600_000) / utf8.encode(unit).length)) + "\x1b[201~";
    };

    const checks = () => {
      sync();
      const pending = acceptedBytes - okBytes - droppedBytes;
      if (pending > MAX_PENDING) violate(`pending ${pending} bytes exceeds the bound`);
      const current = inflight as InFlight | null;
      if (fatal === null && current !== null) {
        const leastItHad = counted.min - current.maxAtStart;
        if (leastItHad >= HARD + TOLERANCE) violate(`no fatal stop after at least ${Math.round(leastItHad)} ms counted with no answer`);
        if (leastItHad >= SOFT + TOLERANCE && !slowActive) violate(`no slow notice after at least ${Math.round(leastItHad)} ms counted`);
      }
      if ((fatal !== null || current === null) && slowActive) violate("slow notice still shown with no send being waited on");
    };

    for (let op = 0; op < OPS; op++) {
      const roll = random();
      if (roll < 0.55) {
        const text = randomText();
        const lone = random() < 0.02;
        const data = lone ? text + "\ud800" : text;
        const length = utf8.encode(text).length;
        sync();
        const pending = acceptedBytes - okBytes - droppedBytes;
        const roomFor = !lone && fatal === null && length <= MAX_PENDING && data.length <= MAX_PENDING && pending + length <= MAX_PENDING;
        // The queue may send the first chunk before `write` returns, so the
        // stream the sends are checked against has to hold the data already.
        if (roomFor) { accepted += data; acceptedBytes += length; }
        const ok = queue.write(data);
        if (!ok && !roomFor && !lone && fatal === null) stats.refusedForRoom += 1;
        if (ok !== roomFor) {
          violate(`write of ${length} bytes with ${pending} pending: accepted=${ok}, expected ${roomFor} (lone=${lone}, fatal=${fatal !== null})`);
          if (roomFor) { accepted = accepted.slice(0, accepted.length - data.length); acceptedBytes -= length; }
          else if (ok) { accepted += data; acceptedBytes += length; }
        }
      } else if (roll < 0.85) {
        const size = random();
        const ms = size < 0.5 ? Math.floor(random() * 300) : size < 0.85 ? 300 + Math.floor(random() * 5700) : 6000 + Math.floor(random() * 64_000);
        await vi.advanceTimersByTimeAsync(ms);
      } else if (roll < 0.93) {
        // The page's thread blocks: the clock the queue reads jumps, no timer fires.
        const ms = random() < 0.5 ? Math.floor(random() * STALL_THRESHOLD) : 1000 + Math.floor(random() * 15_000);
        sync();
        stall += ms;
        if (!hidden && ms <= STALL_THRESHOLD) counted.max += ms;
      } else {
        sync();
        hidden = !hidden;
        // A tick armed before the toggle credits its whole period to the state it finds.
        counted.min -= TICK; counted.max += TICK;
      }
      await flush();
      checks();
    }

    // Let everything that can settle settle, in view.
    sync();
    if (hidden) { hidden = false; counted.min -= TICK; counted.max += TICK; }
    for (let i = 0; i < 12; i++) { await vi.advanceTimersByTimeAsync(20_000); await flush(); checks(); }
    if (fatal === null && !(dropMark !== null || cursor === accepted.length)) violate(`everything accepted was neither sent nor dropped (cursor ${cursor} of ${accepted.length})`);
    if (slowActive) violate("a slow notice is still showing after everything settled");
    queue.dispose();
    await flush();
  } finally {
    vi.useRealTimers();
  }
  return { violations, stats };
}

describe("terminal input queue against a slow, silent or failing host", () => {
  afterEach(() => { vi.useRealTimers(); });
  const total: Stats = { slowNotices: 0, hardFatals: 0, goneFatals: 0, drops: 0, refusedForRoom: 0 };
  for (const seed of SEEDS) {
    it(`seed ${seed}: ${OPS} operations keep order, bounds and the evidence rule`, async () => {
      const { violations, stats } = await simulate(seed);
      for (const key of Object.keys(total) as Array<keyof Stats>) total[key] += stats[key];
      expect(violations).toEqual([]);
    }, 120_000);
  }
  it("the runs above exercised every path the invariants guard", () => {
    // A fuzz that never reaches the slow notice, the hard bound, a gone answer, a
    // dropped chunk or the 1 MiB refusal proves nothing about them.
    expect(total.slowNotices).toBeGreaterThan(0);
    expect(total.hardFatals).toBeGreaterThan(0);
    expect(total.goneFatals).toBeGreaterThan(0);
    expect(total.drops).toBeGreaterThan(0);
    expect(total.refusedForRoom).toBeGreaterThan(0);
  });
});
