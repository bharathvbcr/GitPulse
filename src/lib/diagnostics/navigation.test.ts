import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installNavigationDiagnostics, QUIET_MS, SETTLE_LIMIT_MS, SLOW_NAVIGATION_MS } from "./navigation";

describe("navigation diagnostics", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  function probe(visible = true) {
    let time = 0;
    const frames: Array<() => void> = [];
    let onChange: (() => void) | null = null;
    const disconnect = vi.fn(() => { onChange = null; });
    const warn = vi.fn();
    const nav = installNavigationDiagnostics({ warn }, {
      now: () => time,
      frame: (callback) => { frames.push(callback); },
      observe: (callback) => { onChange = callback; return disconnect; },
      visible: () => visible,
    });
    return {
      nav, warn, disconnect,
      /** Runs one animation frame `elapsed` ms from now. */
      async frame(elapsed: number) {
        time += elapsed;
        await vi.advanceTimersByTimeAsync(elapsed);
        const due = frames.splice(0);
        for (const callback of due) callback();
      },
      async mutateAfter(elapsed: number) {
        time += elapsed;
        await vi.advanceTimersByTimeAsync(elapsed);
        onChange?.();
      },
      async wait(elapsed: number) {
        time += elapsed;
        await vi.advanceTimersByTimeAsync(elapsed);
      },
      observing: () => onChange !== null,
    };
  }

  it("records nothing for a navigation that settles inside the budget", async () => {
    const p = probe();
    p.nav.navigate("repo-a/work", "repository/work");
    await p.frame(16);
    await p.frame(16);
    await p.mutateAfter(20);
    await p.wait(QUIET_MS + 1);
    await p.wait(60_000);
    expect(p.warn).not.toHaveBeenCalled();
    expect(p.disconnect).toHaveBeenCalledOnce();
  });

  it("times a cold navigation to its last DOM change, not its first frame", async () => {
    const p = probe();
    p.nav.navigate("repo-a/history", "repository/history");
    await p.frame(16);
    await p.frame(16);
    // Hydrate data lands well after the first paint.
    await p.mutateAfter(100);
    await p.mutateAfter(140);
    await p.mutateAfter(140);
    await p.wait(QUIET_MS);
    expect(p.warn).toHaveBeenCalledExactlyOnceWith("performance:nav", expect.any(String));
    const line = p.warn.mock.calls[0][1] as string;
    expect(line).toContain("worst: cold repository/history settled_ms=412 first_frame_ms=32");
    expect(line).toContain("session settled: cold n=1 p50=412 p95=412 max=412 ms, warm n=0");
  });

  it("calls the second visit to a destination warm and keeps both distributions", async () => {
    const p = probe();
    for (const [key, settleAfter] of [["a", 300], ["b", 40], ["a", 260]] as const) {
      p.nav.navigate(key, "repository/code");
      await p.frame(10);
      await p.frame(10);
      await p.mutateAfter(settleAfter - 20);
      await p.wait(QUIET_MS);
    }
    // The first slow one reported at once; the warm slow one waits out the window.
    expect(p.warn).toHaveBeenCalledTimes(1);
    await p.wait(30_000);
    expect(p.warn).toHaveBeenCalledTimes(2);
    const line = p.warn.mock.calls[1][1] as string;
    expect(line).toContain("worst: warm repository/code settled_ms=260");
    expect(line).toContain("cold n=2 p50=40 p95=300 max=300 ms, warm n=1 p50=260 p95=260 max=260 ms");
  });

  it("does not report a superseded navigation as fast or slow, only counts it", async () => {
    const p = probe();
    p.nav.navigate("a", "repository/work");
    await p.frame(10);
    await p.mutateAfter(SLOW_NAVIGATION_MS * 4);
    p.nav.navigate("b", "repository/history");
    await p.frame(10);
    await p.frame(10);
    await p.mutateAfter(SLOW_NAVIGATION_MS);
    await p.wait(QUIET_MS);
    const line = p.warn.mock.calls[0][1] as string;
    expect(line).toContain("cold n=1 ");
    expect(line).toContain("1 superseded before settling (not timed)");
    expect(line).toContain("worst: cold repository/history");
  });

  it("reports a view that never stops changing as unsettled rather than timing it", async () => {
    const p = probe();
    p.nav.navigate("a", "tasks");
    await p.frame(10);
    await p.frame(10);
    for (let elapsed = 20; elapsed < SETTLE_LIMIT_MS; elapsed += 100) await p.mutateAfter(100);
    await p.wait(100);
    const line = p.warn.mock.calls[0][1] as string;
    expect(line).toContain(`1 still changing after ${SETTLE_LIMIT_MS} ms`);
    expect(line).toContain("cold n=0");
    expect(p.observing()).toBe(false);
  });

  it("does not time navigation in a hidden document, but remembers the visit", async () => {
    const p = probe(false);
    p.nav.navigate("a", "repository/work");
    expect(p.observing()).toBe(false);
    await p.wait(60_000);
    expect(p.warn).not.toHaveBeenCalled();
  });
});
