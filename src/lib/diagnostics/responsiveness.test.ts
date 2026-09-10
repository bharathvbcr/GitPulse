import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installResponsivenessDiagnostics } from "./responsiveness";

describe("UI responsiveness diagnostics", () => {
  let stop = () => {};
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => { stop(); vi.useRealTimers(); });

  function probe(initial: DocumentVisibilityState = "visible") {
    const target = Object.assign(new EventTarget(), { visibilityState: initial });
    const frame = new EventTarget();
    const warn = vi.fn();
    let time = 0;
    stop = installResponsivenessDiagnostics({ warn }, {
      document: target, window: frame, now: () => time,
    });
    return {
      target, frame, warn,
      async tick(elapsed = 500) { time += elapsed; await vi.advanceTimersByTimeAsync(500); },
      advance(elapsed: number) { time += elapsed; },
    };
  }

  it("healthy ticks produce no diagnostics and keep one timer", async () => {
    const p = probe();
    for (let i = 0; i < 120; i++) await p.tick();
    expect(p.warn).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(1);
  });

  it("records the first delay at the threshold without claiming a cause", async () => {
    const p = probe();
    await p.tick(749);
    expect(p.warn).not.toHaveBeenCalled();
    await p.tick(750);
    expect(p.warn).toHaveBeenCalledExactlyOnceWith("performance:ui", expect.stringContaining("max_delay_ms=250"));
    expect(p.warn.mock.calls[0][1]).toContain("not a specific cause");
  });

  it("aggregates repeated delays and flushes them on a later healthy tick", async () => {
    const p = probe();
    await p.tick(800);
    await p.tick(900);
    await p.tick(1_200);
    expect(p.warn).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 60; i++) await p.tick();
    expect(p.warn).toHaveBeenCalledTimes(2);
    expect(p.warn.mock.calls[1][1]).toContain("2 delayed UI timer sample(s)");
    expect(p.warn.mock.calls[1][1]).toContain("max_delay_ms=700");
  });

  it("does no hidden-window work and rebases the clock on return", async () => {
    const p = probe("hidden");
    expect(vi.getTimerCount()).toBe(0);
    p.advance(60_000);
    p.target.visibilityState = "visible";
    p.target.dispatchEvent(new Event("visibilitychange"));
    await p.tick();
    expect(p.warn).not.toHaveBeenCalled();
    p.target.visibilityState = "hidden";
    p.target.dispatchEvent(new Event("visibilitychange"));
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not record background throttling when visibility changes before its event", async () => {
    const p = probe();
    p.target.visibilityState = "hidden";
    await p.tick(5_000);
    expect(p.warn).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does no unfocused-window work and rebases the clock on return", async () => {
    // WKWebView coalesces timers to ~1s behind another app even while
    // visibilityState stays "visible". Sampling that as a UI freeze filled
    // Diagnostics with a warning every 30s overnight.
    const p = probe();
    p.frame.dispatchEvent(new Event("blur"));
    expect(vi.getTimerCount()).toBe(0);
    p.advance(60_000);
    p.frame.dispatchEvent(new Event("focus"));
    await p.tick();
    expect(p.warn).not.toHaveBeenCalled();
    p.frame.dispatchEvent(new Event("blur"));
    expect(vi.getTimerCount()).toBe(0);
  });

  it("discards delayed samples when the window loses focus instead of reporting them later", async () => {
    const p = probe();
    await p.tick(800);
    await p.tick(900);
    await p.tick(1_200);
    expect(p.warn).toHaveBeenCalledTimes(1);
    p.frame.dispatchEvent(new Event("blur"));
    p.advance(60_000);
    p.frame.dispatchEvent(new Event("focus"));
    for (let i = 0; i < 60; i++) await p.tick();
    expect(p.warn).toHaveBeenCalledTimes(1);
  });

  it("discards delayed samples when the window is hidden instead of reporting them later", async () => {
    const p = probe();
    await p.tick(800);
    await p.tick(900);
    expect(p.warn).toHaveBeenCalledTimes(1);
    p.target.visibilityState = "hidden";
    p.target.dispatchEvent(new Event("visibilitychange"));
    p.advance(60_000);
    p.target.visibilityState = "visible";
    p.target.dispatchEvent(new Event("visibilitychange"));
    for (let i = 0; i < 60; i++) await p.tick();
    expect(p.warn).toHaveBeenCalledTimes(1);
  });

  it("labels sleep-sized gaps as ambiguous instead of declaring a UI freeze", async () => {
    const p = probe();
    await p.tick(60_500);
    expect(p.warn.mock.calls[0][1]).toContain("max_gap_ms=60000");
    expect(p.warn.mock.calls[0][1]).toContain("may include system sleep");
    expect(p.warn.mock.calls[0][1]).not.toContain("delayed UI timer");
  });

  it("teardown removes timers and listeners and is idempotent", async () => {
    const p = probe();
    stop(); stop();
    p.target.dispatchEvent(new Event("visibilitychange"));
    p.frame.dispatchEvent(new Event("blur"));
    await p.tick(10_000);
    expect(vi.getTimerCount()).toBe(0);
    expect(p.warn).not.toHaveBeenCalled();
  });

  it("is inert without a document", () => {
    stop = installResponsivenessDiagnostics({ warn: vi.fn() }, { document: null });
    expect(vi.getTimerCount()).toBe(0);
  });
});
