import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installResponsivenessDiagnostics } from "./responsiveness";
import { readEventLoopDelay, resetEventLoopDelay } from "../runtime/loadCadence";
import { createActivityLog } from "./activity";

describe("UI responsiveness diagnostics", () => {
  let stop = () => {};
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => { stop(); resetEventLoopDelay(); vi.useRealTimers(); });

  function probe(initial: DocumentVisibilityState = "visible") {
    const target = Object.assign(new EventTarget(), { visibilityState: initial });
    const frame = new EventTarget();
    const warn = vi.fn();
    let time = 0;
    const activity = createActivityLog(() => time);
    let view = "repository/history";
    stop = installResponsivenessDiagnostics({ warn }, {
      document: target, window: frame, now: () => time, activity,
      view: () => view,
    });
    return {
      target, frame, warn, activity,
      setView(next: string) { view = next; },
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

  it("records the first delay at the threshold, attributed to the view when nothing else ran", async () => {
    const p = probe();
    await p.tick(749);
    expect(p.warn).not.toHaveBeenCalled();
    await p.tick(750);
    expect(p.warn).toHaveBeenCalledExactlyOnceWith("performance:ui", expect.stringContaining("max_delay_ms=250"));
    const line = p.warn.mock.calls[0][1] as string;
    expect(line).toContain("cause=view:repository/history");
    expect(line).toContain("correlation rather than a profile");
  });

  it("attributes a late sample to the IPC answer handled inside the late interval", async () => {
    const p = probe();
    // Delivered before the probe re-armed: not part of the next interval.
    p.activity.noteCommandSettled("cmd_before_interval");
    await p.tick();
    p.advance(100);
    p.activity.noteCommandSettled("cmd_get_commit_graph");
    p.activity.noteCommandSettled("cmd_get_commit_graph");
    p.activity.noteWatcherEvent();
    p.setView("repository/code");
    await p.tick(800);
    const line = p.warn.mock.calls[0][1] as string;
    expect(line).toContain("worst: cause=command:cmd_get_commit_graph view=repository/code");
    expect(line).toContain("commands=[cmd_get_commit_graphx2]");
    expect(line).toContain("watcher_events=1");
    expect(line).toContain("causes: command:cmd_get_commit_graphx1");
    expect(line).not.toContain("cmd_before_interval");
  });

  it("names a watcher burst and tallies causes across one report", async () => {
    const p = probe();
    await p.tick(800);
    p.advance(10);
    for (let i = 0; i < 5; i++) p.activity.noteWatcherEvent();
    await p.tick(1_500);
    p.advance(10);
    p.activity.noteCommandSettled("cmd_get_status");
    await p.tick(900);
    for (let i = 0; i < 60; i++) await p.tick();
    const line = p.warn.mock.calls[1][1] as string;
    expect(line).toContain("2 delayed UI timer sample(s)");
    expect(line).toContain("worst: cause=watcher-burst");
    expect(line).toContain("watcher_events=5");
    expect(line).toContain("causes: command:cmd_get_statusx1, watcher-burstx1");
  });

  it("keeps sampling when the view getter throws", async () => {
    const target = Object.assign(new EventTarget(), { visibilityState: "visible" as DocumentVisibilityState });
    const warn = vi.fn();
    let time = 0;
    stop = installResponsivenessDiagnostics({ warn }, {
      document: target, window: new EventTarget(), now: () => time,
      activity: createActivityLog(() => time),
      view: () => { throw new Error("store gone"); },
    });
    time += 800;
    await vi.advanceTimersByTimeAsync(500);
    expect(warn.mock.calls[0][1]).toContain("cause=view:unknown");
    expect(vi.getTimerCount()).toBe(1);
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

  it("publishes event-loop delay for schedulers and drops it when the window blurs", async () => {
    const p = probe();
    await p.tick(750);
    expect(readEventLoopDelay()).toBeGreaterThanOrEqual(250);
    p.frame.dispatchEvent(new Event("blur"));
    expect(readEventLoopDelay()).toBe(0);
  });

  it("lets healthy ticks decay a stall instead of keeping the stretched period forever", async () => {
    const p = probe();
    await p.tick(750);
    expect(readEventLoopDelay()).toBeGreaterThanOrEqual(250);
    for (let i = 0; i < 8; i++) await p.tick();
    expect(readEventLoopDelay()).toBeLessThan(250);
  });

  it("is inert without a document", () => {
    stop = installResponsivenessDiagnostics({ warn: vi.fn() }, { document: null });
    expect(vi.getTimerCount()).toBe(0);
  });
});
