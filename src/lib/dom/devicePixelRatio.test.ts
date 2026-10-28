import { describe, expect, it } from "vitest";
import { watchDevicePixelRatio, type PixelRatioWindow } from "./devicePixelRatio";

/** A window whose ratio the test moves, with a media-query list that reports each move once, as a browser does. */
function fakeWindow(initial = 1) {
  const win = { devicePixelRatio: initial } as { devicePixelRatio: number };
  const queries: string[] = [];
  const live = new Set<() => void>();
  const matchMedia = (query: string) => {
    queries.push(query);
    return {
      addEventListener: (_type: string, listener: () => void) => { live.add(listener); },
      removeEventListener: (_type: string, listener: () => void) => { live.delete(listener); },
    };
  };
  const move = (ratio: number) => {
    win.devicePixelRatio = ratio;
    // The old query stops matching: its listeners fire once.
    for (const listener of [...live]) listener();
  };
  return { win: Object.assign(win, { matchMedia }) as unknown as PixelRatioWindow, move, queries, listeners: () => live.size };
}

describe("watchDevicePixelRatio", () => {
  it("arms a query on the current resolution", () => {
    const w = fakeWindow(2);
    watchDevicePixelRatio(() => {}, w.win);
    expect(w.queries).toEqual(["(resolution: 2dppx)"]);
    expect(w.listeners()).toBe(1);
  });

  it("reports each move with the new ratio and re-arms for it", () => {
    const w = fakeWindow(1);
    const seen: number[] = [];
    watchDevicePixelRatio((ratio) => seen.push(ratio), w.win);
    w.move(2);
    w.move(1.5);
    w.move(1);
    expect(seen).toEqual([2, 1.5, 1]);
    expect(w.queries).toEqual(["(resolution: 1dppx)", "(resolution: 2dppx)", "(resolution: 1.5dppx)", "(resolution: 1dppx)"]);
    expect(w.listeners()).toBe(1);
  });

  it("stops: nothing fires and nothing stays armed", () => {
    const w = fakeWindow(1);
    const seen: number[] = [];
    const stop = watchDevicePixelRatio((ratio) => seen.push(ratio), w.win);
    w.move(2);
    stop();
    expect(w.listeners()).toBe(0);
    w.move(3);
    expect(seen).toEqual([2]);
    stop();
  });

  it("does nothing, and says nothing, without a window or without matchMedia", () => {
    expect(() => watchDevicePixelRatio(() => {}, undefined)()).not.toThrow();
    expect(() => watchDevicePixelRatio(() => {}, { devicePixelRatio: 1 } as unknown as PixelRatioWindow)()).not.toThrow();
  });
});
