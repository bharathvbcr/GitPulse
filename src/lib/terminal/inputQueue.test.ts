import { describe, expect, it, vi } from "vitest";
import { createTerminalInput } from "./inputQueue";

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
  it("never retries a failed write or delivers queued input after failure", async () => {
    const send = vi.fn(async () => { throw new Error("broken pipe"); }), error = vi.fn();
    const queue = createTerminalInput(send, error);
    queue.write("a"); queue.write("b");
    await queue.idle();
    expect(send).toHaveBeenCalledTimes(1);
    expect(error).toHaveBeenCalledWith(expect.stringContaining("broken pipe"), true);
    expect(queue.write("c")).toBe(false);
  });
  it("drops queued input on disposal even when an old write later completes", async () => {
    let finish: (() => void) | undefined;
    const send = vi.fn(() => new Promise<void>((r) => { finish = r; }));
    const queue = createTerminalInput(send, vi.fn());
    queue.write("a"); queue.write("b"); queue.dispose(); finish?.();
    await queue.idle();
    expect(send).toHaveBeenCalledTimes(1);
  });
  it("bounds a hung write without sending subsequent data", async () => {
    vi.useFakeTimers();
    try {
      const error = vi.fn();
      const queue = createTerminalInput(() => new Promise<void>(() => {}), error);
      queue.write("a"); queue.write("b");
      await vi.advanceTimersByTimeAsync(5000); await tick();
      expect(error).toHaveBeenCalledWith(expect.stringContaining("timed out"), true);
      await queue.idle();
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
