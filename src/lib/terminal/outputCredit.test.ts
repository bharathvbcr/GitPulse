import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import {
  ACK_FLUSH_BACKSTOP_MS,
  ACK_FLUSH_THRESHOLD,
  createAckCoalescer,
  createOutputCredit,
  frameAckScheduler,
  decodeTerminalOutput,
  MAX_TERMINAL_OUTPUT_CHUNK,
  OUTPUT_CREDIT_WINDOW,
  planTerminalOutput,
  reservedTerminalBytes,
} from "./outputCredit";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const flowRs = readFileSync(join(repoRoot, "src-tauri", "src", "terminal", "flow.rs"), "utf8");

const open = { terminal: true, disposed: false };
const gone = { terminal: true, disposed: true };
const waiting = { terminal: false, disposed: false };

function chunk(n: number, fill = 1): Uint8Array {
  return new Uint8Array(n).fill(fill);
}

describe("terminal output credit", () => {
  it("matches the Rust output window", () => {
    const match = flowRs.match(/const OUTPUT_WINDOW:\s*usize\s*=\s*([^;]+);/);
    expect(match, "OUTPUT_WINDOW declaration not found").not.toBeNull();
    expect(OUTPUT_CREDIT_WINDOW).toBe(Function(`"use strict"; return (${match?.[1]});`)());
  });

  it("decodes standard base64 and refuses anything else", () => {
    const bytes = chunk(5, 65);
    expect(decodeTerminalOutput(btoa(String.fromCharCode(...bytes)))).toEqual(bytes);
    expect(decodeTerminalOutput("")).toEqual(new Uint8Array());
    expect(() => decodeTerminalOutput("!!!!")).toThrow();
    expect(() => decodeTerminalOutput("a")).toThrow();
  });

  it("paints when the emulator is open and acknowledges that chunk once", () => {
    const credit = createOutputCredit();
    const decision = credit.accept(chunk(4), "native-a", open);
    expect(decision.action).toBe("paint");
    if (decision.action !== "paint") return;
    expect(decision.token.bytes).toHaveLength(4);
    expect(decision.token.release()).toBe(4);
    expect(decision.token.release()).toBe(0);
    expect(credit.pending()).toBe(0);
    expect(credit.releaseAll()).toEqual([]);
  });

  it("acknowledges immediately when the view is already gone, without painting", () => {
    const credit = createOutputCredit();
    expect(credit.accept(chunk(3), "native-a", gone)).toEqual({ action: "ack", sessionId: "native-a", bytes: 3 });
    expect(credit.pending()).toBe(0);
    expect(credit.flush()).toEqual([]);
  });

  it("holds output until an emulator exists, then paints the same bytes", () => {
    const credit = createOutputCredit();
    const bytes = chunk(8, 90);
    expect(credit.accept(bytes, "native-a", waiting).action).toBe("held");
    expect(credit.pending()).toBe(8);
    const tokens = credit.flush();
    expect(tokens).toHaveLength(1);
    expect(tokens[0].bytes).toBe(bytes);
    expect(tokens[0].release()).toBe(8);
    expect(credit.pending()).toBe(0);
  });

  it("does not acknowledge an empty chunk", () => {
    const credit = createOutputCredit();
    expect(credit.accept(new Uint8Array(), "native-a", open)).toEqual({ action: "ack", sessionId: "native-a", bytes: 0 });
    expect(credit.pending()).toBe(0);
  });

  it("releases a full window of unpainted output exactly once when the view never opens", () => {
    const credit = createOutputCredit();
    const piece = chunk(4096);
    const pieces = OUTPUT_CREDIT_WINDOW / piece.length;
    for (let i = 0; i < pieces; i++) {
      expect(credit.accept(piece, "native-a", waiting).action).toBe("held");
    }
    expect(credit.pending()).toBe(OUTPUT_CREDIT_WINDOW);
    const overflow = credit.accept(piece, "native-a", waiting);
    expect(overflow.action).toBe("overflow");
    if (overflow.action !== "overflow") return;
    expect(overflow.owed).toEqual([{ sessionId: "native-a", bytes: OUTPUT_CREDIT_WINDOW + piece.length }]);
    expect(credit.pending()).toBe(0);
    expect(credit.releaseAll()).toEqual([]);
  });

  it("keeps two sessions' credit apart and does not ack a released generation twice", () => {
    const credit = createOutputCredit();
    const first = credit.accept(chunk(2), "a", open);
    const second = credit.accept(chunk(5), "b", open);
    expect(first.action).toBe("paint");
    expect(second.action).toBe("paint");
    if (first.action !== "paint" || second.action !== "paint") return;
    expect(first.token.release()).toBe(2);
    const owed = credit.releaseAll();
    expect(owed).toEqual([{ sessionId: "b", bytes: 5 }]);
    expect(second.token.release()).toBe(0);
    expect(credit.pending()).toBe(0);
  });

  it("accounts for 10000 chunks with a single release of whatever is still outstanding", () => {
    const credit = createOutputCredit();
    const one = chunk(1);
    const tokens = [];
    for (let i = 0; i < 10000; i++) {
      const decision = credit.accept(one, i % 2 === 0 ? "a" : "b", open);
      expect(decision.action).toBe("paint");
      if (decision.action === "paint") tokens.push(decision.token);
    }
    let acked = 0;
    for (let i = 0; i < 5000; i++) acked += tokens[i].release();
    for (const item of credit.releaseAll()) acked += item.bytes;
    for (const token of tokens) acked += token.release();
    expect(acked).toBe(10000);
    expect(credit.pending()).toBe(0);
  });
});

describe("reserved output length", () => {
  const modRs = readFileSync(join(repoRoot, "src-tauri", "src", "terminal", "mod.rs"), "utf8");

  it("matches the Rust read size", () => {
    const match = modRs.match(/const OUTPUT_CHUNK_BYTES:\s*usize\s*=\s*(\d+)\s*;/);
    expect(match, "OUTPUT_CHUNK_BYTES declaration not found").not.toBeNull();
    expect(MAX_TERMINAL_OUTPUT_CHUNK).toBe(Number(match?.[1]));
    expect(reservedTerminalBytes(MAX_TERMINAL_OUTPUT_CHUNK)).toBe(MAX_TERMINAL_OUTPUT_CHUNK);
    for (const value of [undefined, null, 0, -1, 1.5, MAX_TERMINAL_OUTPUT_CHUNK + 1, Number.NaN]) {
      expect(reservedTerminalBytes(value)).toBeNull();
    }
  });

  it("acknowledges a corrupt chunk by the length the reader reserved", () => {
    const credit = createOutputCredit();
    const plan = planTerminalOutput(credit, "!!!!", "native-a", 4, open);
    expect(plan).toMatchObject({ action: "ack", bytes: 4 });
    if (plan.action !== "ack") return;
    expect(plan.failure).toContain("Invalid terminal output");
    expect(credit.pending()).toBe(0);
    expect(credit.flush()).toEqual([]);
  });

  it("releases a thousand corrupt chunks and nothing more", () => {
    const credit = createOutputCredit();
    let acked = 0;
    for (let i = 0; i < 1000; i++) {
      const plan = planTerminalOutput(credit, "!!!!", i % 2 === 0 ? "a" : "b", 3, open);
      expect(plan.action).toBe("ack");
      if (plan.action === "ack") acked += plan.bytes;
    }
    expect(acked).toBe(3000);
    expect(credit.pending()).toBe(0);
  });

  it("acknowledges the reserved length when the decoded size disagrees, and does not paint", () => {
    const credit = createOutputCredit();
    const plan = planTerminalOutput(credit, "QQ==", "native-a", 4, open);
    expect(plan).toMatchObject({ action: "ack", bytes: 4 });
    if (plan.action !== "ack") return;
    expect(plan.failure).toContain("reserved credit");
    expect(credit.pending()).toBe(0);
  });

  it("stops when a corrupt chunk does not name a length that can be acknowledged", () => {
    const credit = createOutputCredit();
    for (const reserved of [null, 0, 1.5, MAX_TERMINAL_OUTPUT_CHUNK + 1]) {
      const plan = planTerminalOutput(credit, "!!!!", "native-a", reserved, open);
      expect(plan.action).toBe("stop");
      if (plan.action === "stop") expect(plan.failure).toContain("Invalid terminal output");
    }
    const full = planTerminalOutput(credit, "!!!!", "native-a", MAX_TERMINAL_OUTPUT_CHUNK, open);
    expect(full).toMatchObject({ action: "ack", bytes: MAX_TERMINAL_OUTPUT_CHUNK });
    expect(credit.pending()).toBe(0);
  });

  it("paints a valid chunk when the reserved length matches, and when an older event omitted it", () => {
    const credit = createOutputCredit();
    const matched = planTerminalOutput(credit, "QQ==", "native-a", 1, open);
    expect(matched.action).toBe("paint");
    if (matched.action !== "paint") return;
    expect(matched.token.release()).toBe(1);
    const omitted = planTerminalOutput(credit, "Qg==", "native-a", null, open);
    expect(omitted.action).toBe("paint");
    if (omitted.action !== "paint") return;
    expect(omitted.token.bytes).toEqual(new Uint8Array([66]));
    expect(omitted.token.release()).toBe(1);
    expect(credit.pending()).toBe(0);
  });
});

describe("acknowledgement coalescing", () => {
  function harness() {
    const sent: Array<[string, number]> = [];
    const queued: Array<() => void> = [];
    let cancelled = 0;
    const acks = createAckCoalescer(
      (sessionId, bytes) => sent.push([sessionId, bytes]),
      (flush) => {
        queued.push(flush);
        return () => {
          cancelled += 1;
        };
      },
    );
    const frame = () => queued.splice(0).forEach((flush) => flush());
    return { acks, sent, frame, scheduled: () => queued.length, cancelled: () => cancelled };
  }

  it("sends one acknowledgement per session per frame for many chunks", () => {
    const h = harness();
    for (let i = 0; i < 10; i++) h.acks.add("a", MAX_TERMINAL_OUTPUT_CHUNK);
    h.acks.add("b", 3);
    expect(h.sent, "nothing goes out before the frame").toEqual([]);
    expect(h.scheduled(), "one frame is asked for, not one per chunk").toBe(1);
    h.frame();
    expect(h.sent).toEqual([
      ["a", 10 * MAX_TERMINAL_OUTPUT_CHUNK],
      ["b", 3],
    ]);
    expect(h.acks.owed()).toBe(0);
  });

  it("ignores zero and negative counts and schedules nothing for them", () => {
    const h = harness();
    h.acks.add("a", 0);
    h.acks.add("a", -4);
    expect(h.scheduled()).toBe(0);
    h.acks.flush();
    expect(h.sent).toEqual([]);
  });

  it("sends at once when a session owes the threshold, well inside the window", () => {
    expect(ACK_FLUSH_THRESHOLD).toBeLessThanOrEqual(OUTPUT_CREDIT_WINDOW / 4);
    const h = harness();
    const chunks = ACK_FLUSH_THRESHOLD / MAX_TERMINAL_OUTPUT_CHUNK;
    for (let i = 0; i < chunks - 1; i++) h.acks.add("a", MAX_TERMINAL_OUTPUT_CHUNK);
    expect(h.sent).toEqual([]);
    h.acks.add("a", MAX_TERMINAL_OUTPUT_CHUNK);
    expect(h.sent).toEqual([["a", ACK_FLUSH_THRESHOLD]]);
    expect(h.cancelled(), "the pending frame is cancelled by the early send").toBe(1);
    expect(h.acks.owed()).toBe(0);
    // The stale frame firing anyway sends nothing twice.
    h.frame();
    expect(h.sent).toHaveLength(1);
  });

  it("never holds more than the threshold unsent, however long frames take", () => {
    const h = harness();
    let maxOwed = 0;
    let total = 0;
    for (let i = 0; i < 1000; i++) {
      h.acks.add(i % 3 === 0 ? "b" : "a", MAX_TERMINAL_OUTPUT_CHUNK);
      total += MAX_TERMINAL_OUTPUT_CHUNK;
      maxOwed = Math.max(maxOwed, h.acks.owed());
    }
    h.acks.flush();
    expect(maxOwed).toBeLessThan(2 * ACK_FLUSH_THRESHOLD);
    for (const [, bytes] of h.sent) expect(bytes).toBeLessThanOrEqual(ACK_FLUSH_THRESHOLD);
    expect(h.sent.reduce((sum, [, bytes]) => sum + bytes, 0), "no credit lost or doubled").toBe(total);
  });

  it("flushes synchronously on teardown, so released credit is not left behind", () => {
    const h = harness();
    const credit = createOutputCredit();
    const painted = credit.accept(chunk(100), "a", open);
    credit.accept(chunk(7), "a", waiting);
    if (painted.action !== "paint") throw new Error("expected paint");
    h.acks.add("a", painted.token.release());
    for (const owed of credit.releaseAll()) h.acks.add(owed.sessionId, owed.bytes);
    h.acks.flush();
    expect(h.sent).toEqual([["a", 107]]);
    expect(h.cancelled()).toBe(1);
    h.frame();
    expect(h.sent).toHaveLength(1);
  });

  it("schedules again after a frame has sent", () => {
    const h = harness();
    h.acks.add("a", 1);
    h.frame();
    h.acks.add("a", 2);
    expect(h.scheduled()).toBe(1);
    h.frame();
    expect(h.sent).toEqual([
      ["a", 1],
      ["a", 2],
    ]);
  });

  it("falls back to a timer when no animation frame comes (a hidden window)", () => {
    vi.useFakeTimers();
    try {
      const flush = vi.fn();
      const cancel = frameAckScheduler(flush);
      vi.advanceTimersByTime(ACK_FLUSH_BACKSTOP_MS - 1);
      expect(flush).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1);
      expect(flush).toHaveBeenCalledTimes(1);
      cancel();
      const never = vi.fn();
      frameAckScheduler(never)();
      vi.advanceTimersByTime(ACK_FLUSH_BACKSTOP_MS * 2);
      expect(never).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});
