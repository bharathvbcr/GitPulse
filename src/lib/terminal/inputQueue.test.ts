import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { SESSION_GONE_MARKERS, createTerminalInput, inputIsDead, sessionIsGone } from "./inputQueue";

const tick = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

describe("terminal input transport", () => {
  it("delivers a large bracketed Unicode paste in order without splitting UTF-8 characters", async () => {
    const chunks: string[] = [];
    const queue = createTerminalInput(async (chunk) => { chunks.push(chunk); }, vi.fn());
    const paste = "\x1b[200~" + "世界🚀é".repeat(25000) + "\x1b[201~";
    expect(queue.write(paste)).toBe(true);
    expect(queue.write("after")).toBe(true);
    await queue.idle();
    expect(chunks.join("")).toBe(paste + "after");
    expect(chunks.length).toBeGreaterThan(1);
    for (const chunk of chunks) {
      expect(new TextEncoder().encode(chunk).length).toBeLessThanOrEqual(16384);
      expect(chunk).not.toContain("�");
    }
  });
  it("serializes 10000 keystrokes while a write is blocked", async () => {
    let finish: (() => void) | undefined;
    const chunks: string[] = [];
    const queue = createTerminalInput(async (s) => {
      chunks.push(s);
      if (chunks.length === 1) await new Promise<void>((r) => { finish = r; });
    }, vi.fn());
    queue.write("start");
    for (let i = 0; i < 10000; i++) queue.write(String(i % 10));
    expect(chunks).toEqual(["start"]);
    finish?.();
    await queue.idle();
    expect(chunks.join("")).toBe("start" + "0123456789".repeat(1000));
  });
  it("rejects an oversized paste atomically and visibly", async () => {
    const send = vi.fn(async () => {}), error = vi.fn();
    const queue = createTerminalInput(send, error);
    expect(queue.write("x".repeat(1024 * 1024 + 1))).toBe(false);
    expect(send).not.toHaveBeenCalled();
    expect(error).toHaveBeenCalledWith(expect.stringContaining("1 MiB"), false);
    expect(queue.write("ok")).toBe(true);
    await queue.idle();
    expect(send).toHaveBeenCalledWith("ok", false);
  });
  it("rejects malformed Unicode before any part of a paste crosses JSON IPC", async () => {
    const send = vi.fn(async () => {}), error = vi.fn();
    const queue = createTerminalInput(send, error);
    for (const malformed of ["a\ud800b", "\udc00", "x".repeat(20000) + "\udfff"]) {
      expect(queue.write(malformed)).toBe(false);
    }
    await queue.idle();
    expect(send).not.toHaveBeenCalled();
    expect(error).toHaveBeenCalledWith(expect.stringContaining("Invalid Unicode"), false);
    expect(queue.write("🚀")).toBe(true);
    await queue.idle(); expect(send).toHaveBeenCalledWith("🚀", false);
  });
  it("never retries a failed write or delivers queued input after the session is gone", async () => {
    const send = vi.fn(async () => { throw new Error("Terminal session 'x' not found"); }), error = vi.fn();
    const queue = createTerminalInput(send, error);
    queue.write("a"); queue.write("b");
    await queue.idle();
    expect(send).toHaveBeenCalledTimes(1);
    expect(error).toHaveBeenCalledWith(expect.stringContaining("not found"), true);
    expect(queue.write("c")).toBe(false);
  });
  it("delivers the next write after one rejected send, without retrying or reordering", async () => {
    // The defect: one rejected send (a 2 s write stall) stopped the pump for
    // good and the tab could only be restarted, which kills an agent.
    const delivered: string[] = [];
    let fail = true;
    const send = vi.fn(async (data: string) => {
      if (fail) { fail = false; throw new Error("Terminal input timed out after writing 3 of 6 bytes"); }
      delivered.push(data);
    }), error = vi.fn();
    const queue = createTerminalInput(send, error);
    queue.write("a"); queue.write("b");
    await queue.idle();
    expect(error).toHaveBeenCalledWith(expect.stringContaining("not delivered"), false);
    expect(error).not.toHaveBeenCalledWith(expect.anything(), true);
    expect(delivered).toEqual([]);
    expect(queue.write("c")).toBe(true);
    await queue.idle();
    expect(delivered).toEqual(["c"]);
    expect(send).toHaveBeenCalledTimes(2);
  });
  it("drops queued input on disposal even when an old write later completes", async () => {
    let finish: (() => void) | undefined;
    const send = vi.fn(() => new Promise<void>((r) => { finish = r; }));
    const queue = createTerminalInput(send, vi.fn());
    queue.write("a"); queue.write("b"); queue.dispose(); finish?.();
    await queue.idle();
    expect(send).toHaveBeenCalledTimes(1);
  });
  it("keeps a character and the erases that follow it once each and in order", async () => {
    const chunks: string[] = [];
    const queue = createTerminalInput(async (chunk) => { chunks.push(chunk); }, vi.fn());
    expect(queue.write("ab")).toBe(true);
    expect(queue.write("\x7f")).toBe(true);
    expect(queue.write("\x7f")).toBe(true);
    await queue.idle();
    const joined = chunks.join("");
    expect(joined).toBe("ab\x7f\x7f");
    expect(joined.split("\x7f").length - 1).toBe(2);
  });
  it("bounds a hung write without sending subsequent data", async () => {
    // The bound moved from 5 s to 60 s with the liveness rework (see the
    // "slow host" suite): 5 s is when the user is told, not when input dies.
    vi.useFakeTimers();
    try {
      const error = vi.fn(), send = vi.fn(() => new Promise<void>(() => {}));
      const queue = createTerminalInput(send, error);
      queue.write("a"); queue.write("b");
      await vi.advanceTimersByTimeAsync(61_000); await tick();
      expect(error).toHaveBeenCalledWith(expect.stringContaining("no answer"), true);
      expect(send).toHaveBeenCalledTimes(1);
      await queue.idle();
    } finally { vi.useRealTimers(); }
  });
});

/**
 * A clock the test can stall. `frozen` models a blocked JS thread: wall time
 * passes (the fake timers advance) but nothing the page can read moves until it
 * is released, when it jumps over everything it missed.
 */
function stallableClock() {
  const clock = {
    frozenAt: null as number | null,
    hiddenNow: false,
    now: () => clock.frozenAt ?? performance.now(),
    hidden: () => clock.hiddenNow,
    /** The page blocked for `ms`: timers run on the fake clock, the page sees no time pass, then all of it at once. */
    async block(ms: number) {
      clock.frozenAt = performance.now();
      await vi.advanceTimersByTimeAsync(ms);
      // Released: the page's clock reads the real elapsed time again.
      clock.frozenAt = null;
    },
  };
  return clock;
}

describe("terminal input on a slow host", () => {
  const later = async (ms: number) => { await vi.advanceTimersByTimeAsync(ms); await tick(); };

  it("a send that answers at 6 s does not stop the queue; what was typed meanwhile goes out after it, once", async () => {
    // The defect: any send slower than a 5 s wall-clock deadline ended the tab's
    // input for good ("restart before typing again"), and Restart kills the
    // process, an agent included, because the host was slow for 5 seconds.
    vi.useFakeTimers();
    try {
      const events: string[] = [], onError = vi.fn(), onSlow = vi.fn((m: string | null) => events.push(m ? "slow" : "clear"));
      let n = 0;
      const send = vi.fn((data: string) => {
        events.push(`send:${data}`);
        return n++ === 0 ? new Promise<void>((r) => setTimeout(() => { events.push("answered:a"); r(); }, 6000)) : Promise.resolve();
      });
      const queue = createTerminalInput(send, onError, { onSlow });
      queue.write("a");
      await later(1000);
      queue.write("b");
      await later(4200);
      expect(onSlow).toHaveBeenCalledTimes(1);
      expect(onSlow).toHaveBeenLastCalledWith(expect.stringContaining("slowly"));
      expect(send).toHaveBeenCalledTimes(1);
      await later(1000);
      expect(events).toEqual(["send:a", "slow", "answered:a", "clear", "send:b"]);
      expect(events.indexOf("answered:a")).toBeLessThan(events.indexOf("send:b"));
      expect(events.filter((e) => e === "slow")).toHaveLength(1);
      expect(onSlow).toHaveBeenLastCalledWith(null);
      expect(onError).not.toHaveBeenCalled();
      expect(send.mock.calls.map((c) => c[0])).toEqual(["a", "b"]);
      await queue.idle();
      expect(queue.write("c")).toBe(true);
    } finally { vi.useRealTimers(); }
  });

  it("a send that never answers is fatal only at the hard bound, and says what is actually known", async () => {
    vi.useFakeTimers();
    try {
      const onError = vi.fn(), send = vi.fn(() => new Promise<void>(() => {}));
      const queue = createTerminalInput(send, onError, { onSlow: vi.fn() });
      queue.write("a"); queue.write("b");
      await later(59_000);
      expect(onError).not.toHaveBeenCalled();
      expect(queue.write("c")).toBe(true);
      await later(2_000);
      expect(onError).toHaveBeenCalledTimes(1);
      const [message, fatal] = onError.mock.calls[0];
      expect(fatal).toBe(true);
      expect(message).toContain("no answer from the terminal backend for 60 s");
      expect(message).toContain("overloaded");
      expect(message).not.toMatch(/session (died|ended|is gone)/i);
      expect(send).toHaveBeenCalledTimes(1);
      expect(queue.write("d")).toBe(false);
      await queue.idle();
    } finally { vi.useRealTimers(); }
  });

  it("does not count time the page itself was blocked as backend latency", async () => {
    vi.useFakeTimers();
    try {
      const clock = stallableClock(), onError = vi.fn(), onSlow = vi.fn();
      let answer!: () => void;
      const send = vi.fn(() => new Promise<void>((r) => { answer = r; }));
      const queue = createTerminalInput(send, onError, { onSlow, clock });
      queue.write("a");
      await later(1000);
      // An 8 s block, with the answer arriving only after it. By wall clock
      // that send took 9.2 s; the backend had 1.2 s of it.
      await clock.block(8000);
      await later(250);
      answer(); await later(0);
      await queue.idle();
      expect(onSlow).not.toHaveBeenCalled();
      expect(onError).not.toHaveBeenCalled();
      expect(queue.write("b")).toBe(true);
    } finally { vi.useRealTimers(); }
  });

  it("a send that is slow in earnest still reaches the hard bound across repeated page stalls", async () => {
    vi.useFakeTimers();
    try {
      const clock = stallableClock(), onError = vi.fn();
      const queue = createTerminalInput(() => new Promise<void>(() => {}), onError, { onSlow: vi.fn(), clock });
      queue.write("a");
      for (let i = 0; i < 5; i++) { await later(5000); await clock.block(20_000); }
      expect(onError).not.toHaveBeenCalled();
      // 25 s counted so far (five 5 s runs); the 100 s of blocks are not.
      await later(30_000);
      expect(onError).not.toHaveBeenCalled();
      await later(7_000);
      expect(onError).toHaveBeenCalledWith(expect.stringContaining("no answer"), true);
    } finally { vi.useRealTimers(); }
  });

  it("pauses the hard bound while the window is hidden", async () => {
    vi.useFakeTimers();
    try {
      const clock = stallableClock(), onError = vi.fn();
      const queue = createTerminalInput(() => new Promise<void>(() => {}), onError, { onSlow: vi.fn(), clock });
      queue.write("a");
      await later(10_000);
      clock.hiddenNow = true;
      await later(300_000);
      expect(onError).not.toHaveBeenCalled();
      clock.hiddenNow = false;
      await later(45_000);
      expect(onError).not.toHaveBeenCalled();
      await later(6_000);
      expect(onError).toHaveBeenCalledWith(expect.stringContaining("no answer"), true);
    } finally { vi.useRealTimers(); }
  });

  it("retracts the notice when the slow send is rejected, and drops the queue as before", async () => {
    vi.useFakeTimers();
    try {
      const onError = vi.fn(), onSlow = vi.fn();
      const send = vi.fn(() => new Promise<void>((_, reject) => setTimeout(() => reject(new Error("pty write failed")), 7000)));
      const queue = createTerminalInput(send, onError, { onSlow });
      queue.write("a"); queue.write("b");
      await later(8000);
      expect(onSlow.mock.calls.map((c) => c[0] === null)).toEqual([false, true]);
      expect(onError).toHaveBeenCalledWith(expect.stringContaining("not delivered"), false);
      expect(send).toHaveBeenCalledTimes(1);
    } finally { vi.useRealTimers(); }
  });

  it("a slow send whose answer is that the session is gone is fatal on that evidence, not at a bound", async () => {
    vi.useFakeTimers();
    try {
      const onError = vi.fn();
      const send = vi.fn(() => new Promise<void>((_, reject) => setTimeout(() => reject(new Error("Terminal session 'x' not found")), 9000)));
      const queue = createTerminalInput(send, onError, { onSlow: vi.fn() });
      queue.write("a");
      await later(8000);
      expect(onError).not.toHaveBeenCalled();
      await later(2000);
      expect(onError).toHaveBeenCalledWith(expect.stringContaining("not found"), true);
    } finally { vi.useRealTimers(); }
  });

  it("stops watching a send once the queue is disposed", async () => {
    vi.useFakeTimers();
    try {
      const onError = vi.fn(), onSlow = vi.fn();
      const queue = createTerminalInput(() => new Promise<void>(() => {}), onError, { onSlow });
      queue.write("a");
      await later(6000);
      queue.dispose();
      await later(1000);
      await queue.idle();
      expect(vi.getTimerCount()).toBe(0);
      expect(onError).not.toHaveBeenCalled();
      expect(onSlow).toHaveBeenLastCalledWith(null);
    } finally { vi.useRealTimers(); }
  });
});

it("preserves legacy mouse bytes in order alongside UTF-8 typing", async () => {
  const sent: Array<{data:string; binary:boolean}> = [];
  const queue = createTerminalInput(async (data, binary) => { sent.push({data,binary}); }, vi.fn());
  queue.write("界"); queue.write("\x1b[M\x80\xff", true); queue.write("after");
  await queue.idle();
  expect(sent).toEqual([{data:"界",binary:false},{data:"\x1b[M\x80\xff",binary:true},{data:"after",binary:false}]);
});

/**
 * Which backend answers end a tab's input is decided here by text, and the
 * text is written in Rust. Both sides are pinned: the markers are read out of
 * `terminal/mod.rs`, and every answer the terminal lanes and the write and
 * resize paths can give is looked up in its Rust source, so a reworded message
 * fails this test instead of quietly changing which way the frontend reads it.
 */
describe("backend answers the input queue reads as a session that is gone", () => {
  const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
  const rust = (file: string) => readFileSync(join(repoRoot, "src-tauri", "src", "terminal", file), "utf8");

  it("uses Rust's SESSION_GONE_MARKERS, word for word", () => {
    const declared = rust("mod.rs").match(/pub const SESSION_GONE_MARKERS:\s*\[&str;\s*\d+\]\s*=\s*\[([^\]]*)\]/);
    expect(declared, "SESSION_GONE_MARKERS declaration not found").not.toBeNull();
    const markers = [...(declared?.[1] ?? "").matchAll(/"([^"]*)"/g)].map((m) => m[1]);
    expect(markers.length).toBeGreaterThan(0);
    expect([...SESSION_GONE_MARKERS]).toEqual(markers);
  });

  // [file, literal as written in Rust, the session is gone]
  const ANSWERS: Array<[string, string, boolean]> = [
    ["mod.rs", "Terminal session '{session_id}' not found", true],
    ["mod.rs", "Terminal session is closed", true],
    ["input.rs", "Terminal closed after writing {written} of {} input bytes", true],
    ["executor.rs", "The terminal {name} has shut down: the terminal session is closed", true],
    ["executor.rs", "{operation} was dropped before it ran: the terminal session is closed", true],
    ["executor.rs", "The terminal {} queue is full ({} waiting); try again in a moment", false],
    ["executor.rs", "{operation} panicked: {}", false],
    ["executor.rs", "Could not start a terminal {name} thread: {error}", false],
    ["input.rs", "Terminal input timed out after writing {written} of {} bytes; input was not retried", false],
    ["input.rs", "Terminal input made no progress after {written} of {} bytes", false],
    ["input.rs", "Terminal input failed after {written} of {} bytes: {e}", false],
    ["mod.rs", "Terminal input is {} bytes; the per-write limit is {MAX_PTY_INPUT_BYTES}", false],
    ["mod.rs", "Terminal is busy; the resize gave up after {}ms", false],
    ["mod.rs", "Failed to resize terminal: {e}", false],
  ];
  for (const [file, literal, gone] of ANSWERS) {
    it(`${gone ? "gone" : "not gone"}: ${literal}`, () => {
      expect(rust(file), `${file} no longer says this; reclassify it here`).toContain(`"${literal}"`);
      expect(sessionIsGone(literal)).toBe(gone);
      expect(sessionIsGone(new Error(literal))).toBe(gone);
      // Only the hard bound, never a backend answer, is fatal without being gone.
      expect(inputIsDead(literal)).toBe(gone);
    });
  }

  it("a full lane queue is reported and the next keystroke still goes out, in order", async () => {
    const sent: string[] = [];
    const onError = vi.fn();
    let first = true;
    const queue = createTerminalInput(async (data) => {
      if (first) { first = false; throw new Error("The terminal input queue is full (64 waiting); try again in a moment"); }
      sent.push(data);
    }, onError);
    queue.write("lost");
    await queue.idle();
    expect(onError).toHaveBeenCalledWith(expect.stringMatching(/^Some input was not delivered and was dropped\. .*queue is full/), false);
    expect(queue.write("next")).toBe(true);
    queue.write("after");
    await queue.idle();
    expect(sent.join("")).toBe("nextafter");
  });
});
