import { describe, expect, it } from "vitest";
import { mulberry32 } from "../__tests__/prng";
import { createAckCoalescer, createOutputCredit } from "./outputCredit";
import { createSyncFrameWriter, SYNC_HOLD_MAX_BYTES, type SyncFrameScheduler } from "./syncFrames";

/**
 * Property tests for the sync-frame writer.
 *
 * The oracle is the generator, not a second parser: every stream is built from
 * tokens whose effect on DEC 2026 is known by construction, and the generator
 * records, for every byte position, whether a block is open once that many
 * bytes have been parsed. A block opens when the final byte of an effective
 * `CSI ? ... 2026 ... h` is consumed and closes with the final byte of an
 * effective `l`. The properties are then read against that record.
 */

const ESC = 0x1b;
const enc = (s: string): number[] => [...new TextEncoder().encode(s)];

interface Token {
  bytes: number[];
  /** Index (within bytes) of the byte that takes effect, or -1. */
  effectAt: number;
  effect: "open" | "close" | "none";
}

const none = (bytes: number[]): Token => ({ bytes, effectAt: -1, effect: "none" });
const csi = (body: string, effect: "open" | "close" | "none"): Token => {
  const bytes = [ESC, ...enc(`[${body}`)];
  return { bytes, effectAt: effect === "none" ? -1 : bytes.length - 1, effect };
};
const csi8 = (body: string, effect: "open" | "close"): Token => {
  const bytes = [0xc2, 0x9b, ...enc(body)];
  return { bytes, effectAt: bytes.length - 1, effect };
};

function tokenFor(rng: () => number): Token {
  const pick = <T>(items: T[]): T => items[Math.floor(rng() * items.length)];
  const roll = rng();
  if (roll < 0.22) {
    const text = pick(["abc", "hello world\r\n", "é世界🚀", "°C", "\r\n", "x", "[?2026h", "?2026l", "2026"]);
    return none(enc(text));
  }
  if (roll < 0.28) return none([0xc2, 0xb0]); // a C2 lead that is not a C1
  if (roll < 0.36) return csi(pick(["1;31m", "0m", "H", "2J", "38;2;1;2;3m", "?25h", "?25l", "?2004h", "?1049l"]), "none");
  if (roll < 0.52) {
    return pick<Token>([
      csi("?2026h", "open"),
      csi("?1049;2026h", "open"),
      csi("?2026;1h", "open"),
      csi("?02026h", "open"),
      csi("?2026:1h", "open"),
      csi("?;2026h", "open"),
      csi8("?2026h", "open"),
    ]);
  }
  if (roll < 0.68) {
    return pick<Token>([csi("?2026l", "close"), csi("?25;2026l", "close"), csi8("?2026l", "close"), csi("?2026;1l", "close")]);
  }
  if (roll < 0.76) {
    return pick<Token>([
      csi("?1:2026h", "none"),
      csi("2026h", "none"),
      csi("?2026$p", "none"),
      csi("?2026 h", "none"),
      csi("?20260h", "none"),
      none([ESC, ...enc("[?2026"), 0x18, ...enc("h")]),
      none([ESC, ...enc("[?20"), 0x1a, ...enc("26h")]),
    ]);
  }
  if (roll < 0.9) {
    const payload = pick(["[?2026h", "[?2026l", "?2026h", "title", ""]);
    return none(
      pick([
        [ESC, ...enc(`]0;${payload}`), 0x07],
        [ESC, ...enc(`]0;${payload}`), ESC, 0x5c],
        [ESC, ...enc(`]0;${payload}`), 0xc2, 0x9c],
        [ESC, ...enc(`Pq${payload}`), ESC, 0x5c],
        [ESC, ...enc(`_Gi=1;${payload}`), ESC, 0x5c],
        [ESC, ...enc(`^${payload}`), ESC, 0x5c],
      ]),
    );
  }
  return none(enc(pick(["\x1b(B", "\x1b7", "\x1b8", "\x1b=", "\x07"])));
}

interface Stream {
  bytes: Uint8Array;
  /** open[p]: a block is open after p bytes have been parsed. Length bytes.length + 1. */
  open: boolean[];
  /** For the block open at the end: index of the first byte of its opening sequence. */
  lastOpenTokenStart: number;
  /** Index of the final byte of that opening sequence. */
  lastOpenEffectAt: number;
}

function makeStream(rng: () => number, tokens: number): Stream {
  const bytes: number[] = [];
  const open: boolean[] = [false];
  let isOpen = false;
  let lastOpenTokenStart = -1;
  let lastOpenEffectAt = -1;
  for (let t = 0; t < tokens; t++) {
    const token = tokenFor(rng);
    const base = bytes.length;
    for (let i = 0; i < token.bytes.length; i++) {
      bytes.push(token.bytes[i]);
      if (i === token.effectAt) {
        if (token.effect === "open" && !isOpen) {
          isOpen = true;
          lastOpenTokenStart = base;
          lastOpenEffectAt = base + i;
        } else if (token.effect === "close") isOpen = false;
      }
      open.push(isOpen);
    }
  }
  return { bytes: Uint8Array.from(bytes), open, lastOpenTokenStart, lastOpenEffectAt };
}

function splitAt(bytes: Uint8Array, cuts: number[]): Uint8Array[] {
  const out: Uint8Array[] = [];
  let at = 0;
  for (const cut of [...cuts, bytes.length]) {
    out.push(bytes.subarray(at, cut));
    at = cut;
  }
  return out;
}

function randomCuts(rng: () => number, length: number): number[] {
  const mode = rng();
  const cuts = new Set<number>();
  if (mode < 0.2) for (let i = 1; i < length; i++) cuts.add(i); // byte at a time
  else {
    const n = Math.floor(rng() * Math.min(12, length + 1));
    for (let i = 0; i < n; i++) cuts.add(Math.floor(rng() * (length + 1)));
  }
  return [...cuts].filter((c) => c > 0 && c < length).sort((a, b) => a - b);
}

interface RunOutcome {
  writes: Uint8Array[];
  writtenBeforeFlush: number;
  heldAtEnd: number;
  sent: number;
  received: number;
  pendingCredit: number;
}

/** A fake scheduler that never fires: only the cap and explicit flushes are in play. */
const idle: SyncFrameScheduler = { set: () => 0, clear: () => {} };

function run(chunks: Uint8Array[], opts: { deferCompletion: boolean; rng: () => number; ending: "flush" | "discard" }): RunOutcome {
  const writes: Uint8Array[] = [];
  const callbacks: (() => void)[] = [];
  const writer = createSyncFrameWriter({
    write(bytes, written) {
      writes.push(bytes);
      if (opts.deferCompletion) callbacks.push(written);
      else written();
    },
    scheduler: idle,
  });
  let sent = 0;
  const acks = createAckCoalescer((_, n) => (sent += n), () => () => {});
  const credit = createOutputCredit();
  let received = 0;
  for (const chunk of chunks) {
    received += chunk.length;
    const decision = credit.accept(chunk, "s", { terminal: true, disposed: false });
    if (decision.action === "ack") {
      acks.add("s", decision.bytes);
      continue;
    }
    if (decision.action !== "paint") throw new Error(decision.action);
    const token = decision.token;
    writer.push(token.bytes, () => acks.add("s", token.release()));
    if (opts.deferCompletion && callbacks.length && opts.rng() < 0.5) callbacks.shift()?.(); // in order, like xterm
  }
  const writtenBeforeFlush = writes.reduce((n, w) => n + w.length, 0);
  const heldAtEnd = writer.held();
  if (opts.ending === "flush") writer.flush();
  else {
    writer.discard();
    for (const owed of credit.releaseAll()) acks.add(owed.sessionId, owed.bytes);
  }
  while (callbacks.length) callbacks.shift()?.();
  acks.flush();
  return { writes, writtenBeforeFlush, heldAtEnd, sent, received, pendingCredit: credit.pending() };
}

function concat(parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function chunkStarts(chunks: Uint8Array[]): number[] {
  const starts: number[] = [];
  let at = 0;
  for (const c of chunks) {
    starts.push(at);
    at += c.length;
  }
  return starts;
}

function check(stream: Stream, chunks: Uint8Array[], rng: () => number, label: string) {
  const outcome = run(chunks, { deferCompletion: rng() < 0.5, rng, ending: "flush" });
  // 1. Bytes are not lost, duplicated or reordered.
  expect(Buffer.from(concat(outcome.writes)).equals(Buffer.from(stream.bytes)), `${label}: concatenation`).toBe(true);
  // 2. No write ends inside an open block (no bound can fire: idle timer, small streams).
  let at = 0;
  for (const write of outcome.writes) {
    at += write.length;
    // The flush at the end writes the final held block; every write before it must end outside a block.
    if (at <= outcome.writtenBeforeFlush) expect(stream.open[at], `${label}: write ended at ${at} inside a block`).toBe(false);
  }
  // 3. Nothing is held unless a block is open, and what is held starts where the writer should cut.
  if (!stream.open[stream.bytes.length]) {
    expect(outcome.heldAtEnd, `${label}: held without an open block`).toBe(0);
    expect(outcome.writtenBeforeFlush).toBe(stream.bytes.length);
  } else {
    const starts = chunkStarts(chunks);
    // The chunk in which the opening sequence's final byte was parsed.
    let chunkOfFinal = 0;
    for (let i = 0; i < starts.length; i++) if (starts[i] <= stream.lastOpenEffectAt) chunkOfFinal = i;
    const expectedCut = Math.max(stream.lastOpenTokenStart, starts[chunkOfFinal]);
    expect(outcome.writtenBeforeFlush, `${label}: cut point`).toBe(expectedCut);
    expect(outcome.heldAtEnd).toBe(stream.bytes.length - expectedCut);
  }
  // 4. Credit: everything received is acknowledged once the writes are parsed.
  expect(outcome.sent, `${label}: acked`).toBe(outcome.received);
  expect(outcome.pendingCredit, `${label}: credit still pending`).toBe(0);
}

describe("sync frame writer: properties", () => {
  it("every split point of short streams keeps the invariants", () => {
    const rng = mulberry32(0x2026);
    let streams = 0;
    for (let s = 0; s < 120; s++) {
      const stream = makeStream(rng, 2 + Math.floor(rng() * 4));
      if (stream.bytes.length > 48) continue;
      streams++;
      const n = stream.bytes.length;
      for (let a = 1; a < n; a++) {
        check(stream, splitAt(stream.bytes, [a]), rng, `seed-stream ${s} split ${a}`);
        for (let b = a + 1; b < n; b += 1 + Math.floor(rng() * 3)) check(stream, splitAt(stream.bytes, [a, b]), rng, `stream ${s} split ${a},${b}`);
      }
      check(stream, [stream.bytes], rng, `stream ${s} whole`);
    }
    expect(streams).toBeGreaterThan(40);
  });

  it("random streams in random chunks keep the invariants", () => {
    for (let seed = 1; seed <= 400; seed++) {
      const rng = mulberry32(seed);
      const stream = makeStream(rng, 5 + Math.floor(rng() * 60));
      check(stream, splitAt(stream.bytes, randomCuts(rng, stream.bytes.length)), rng, `seed ${seed}`);
    }
  });

  it("byte-at-a-time delivery holds exactly the open block", () => {
    for (let seed = 1000; seed < 1100; seed++) {
      const rng = mulberry32(seed);
      const stream = makeStream(rng, 5 + Math.floor(rng() * 30));
      const cuts = Array.from({ length: stream.bytes.length - 1 }, (_, i) => i + 1);
      check(stream, splitAt(stream.bytes, cuts), rng, `seed ${seed}`);
    }
  });

  it("discarding at any point acknowledges every byte received", () => {
    for (let seed = 2000; seed < 2300; seed++) {
      const rng = mulberry32(seed);
      const stream = makeStream(rng, 5 + Math.floor(rng() * 40));
      const chunks = splitAt(stream.bytes, randomCuts(rng, stream.bytes.length));
      const outcome = run(chunks.slice(0, Math.floor(rng() * (chunks.length + 1))), { deferCompletion: rng() < 0.5, rng, ending: "discard" });
      expect(outcome.sent, `seed ${seed}`).toBe(outcome.received);
      expect(outcome.pendingCredit).toBe(0);
    }
  });

  it("with the cap in play, held bytes never exceed it and nothing is lost", () => {
    for (let seed = 3000; seed < 3040; seed++) {
      const rng = mulberry32(seed);
      const writes: Uint8Array[] = [];
      const writer = createSyncFrameWriter({ write: (b, w) => (writes.push(b), w()), scheduler: idle, maxHeldBytes: 2048 });
      const sent: number[] = [];
      const all: number[] = [];
      for (let i = 0; i < 60; i++) {
        const stream = makeStream(rng, 3 + Math.floor(rng() * 30));
        const filler = Uint8Array.from({ length: Math.floor(rng() * 1500) }, () => 0x61);
        const chunk = concat([stream.bytes, filler]);
        all.push(...chunk);
        writer.push(chunk, () => sent.push(chunk.length));
        expect(writer.held()).toBeLessThanOrEqual(2048);
      }
      writer.flush();
      expect(Buffer.from(concat(writes)).equals(Buffer.from(all)), `seed ${seed}`).toBe(true);
      expect(sent).toHaveLength(60);
    }
  });

  it("the production cap fits the credit window with the generator's largest holds", () => {
    expect(SYNC_HOLD_MAX_BYTES).toBeLessThanOrEqual(128 * 1024);
  });
});
