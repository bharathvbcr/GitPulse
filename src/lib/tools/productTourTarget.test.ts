import { afterEach, describe, expect, it, vi } from "vitest";
import { FRAME_FALLBACK_MS, observeTourTarget, tourPosition } from "./productTourTarget";

describe("live walkthrough placement", () => {
  const card = { width: 380, height: 420 };
  it("sits below a title-bar control without covering it", () => {
    expect(tourPosition({ left: 140, top: 8, width: 120, height: 32 }, card, 1200, 900))
      .toEqual({ left: 140, top: 56 });
  });
  it("moves above a low target and away from the right edge", () => {
    expect(tourPosition({ left: 1000, top: 750, width: 100, height: 32 }, card, 1200, 900))
      .toEqual({ left: 804, top: 314 });
  });
  it("docks at the lower right when a target is absent", () => {
    expect(tourPosition(null, card, 1200, 900)).toEqual({ left: 804, top: 464 });
  });
  it.each([[320, 400], [200, 160], [0, 0]])("never positions outside a %s by %s viewport", (width, height) => {
    const result = tourPosition({ left: -40, top: 500, width: 800, height: 200 }, card, width, height);
    expect(result.left).toBeGreaterThanOrEqual(0);
    expect(result.top).toBeGreaterThanOrEqual(0);
    expect(result.left).toBeLessThanOrEqual(width);
    expect(result.top).toBeLessThanOrEqual(height);
  });
});

/** A document whose only animation frame is the one nobody ever runs. Measured
 * on a headless Chrome that stopped producing frames mid-run: callbacks queued,
 * none delivered, while timers and the rest of the page kept going. */
function pageWithoutFrames() {
  const frames: Array<() => void> = [];
  let measured = 0;
  const anchor = {
    getClientRects: () => [{}],
    getBoundingClientRect: () => ({ left: 140, top: 8, right: 260, bottom: 40, width: 120, height: 32 }),
    scrollIntoView: () => {},
  };
  class Stub { observe() {} unobserve() {} disconnect() {} }
  vi.stubGlobal("document", { body: {}, querySelectorAll: () => { measured++; return [anchor]; } });
  vi.stubGlobal("getComputedStyle", () => ({ visibility: "visible" }));
  vi.stubGlobal("innerWidth", 1200);
  vi.stubGlobal("innerHeight", 900);
  vi.stubGlobal("window", { addEventListener() {}, removeEventListener() {} });
  vi.stubGlobal("ResizeObserver", Stub);
  vi.stubGlobal("MutationObserver", Stub);
  vi.stubGlobal("requestAnimationFrame", (callback: () => void) => frames.push(callback));
  vi.stubGlobal("cancelAnimationFrame", () => {});
  return { frames, measurements: () => measured };
}

describe("live walkthrough target observation", () => {
  const card = { getBoundingClientRect: () => ({ width: 380, height: 420 }) } as unknown as HTMLElement;
  const observe = (update: (rect: unknown, position: unknown) => void) =>
    observeTourTarget("[data-tour='repository']", card, update);
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

  it("measures the target on a host that has stopped painting", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const page = pageWithoutFrames();
    const update = vi.fn();
    const stop = observe(update);
    expect(page.frames).toHaveLength(1);
    expect(update).not.toHaveBeenCalled();
    vi.advanceTimersByTime(FRAME_FALLBACK_MS);
    expect(update).toHaveBeenCalledWith({ left: 140, top: 8, width: 120, height: 32 }, { left: 140, top: 56 });
    stop();
  });

  it("measures once when the frame arrives first", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const page = pageWithoutFrames();
    const stop = observe(vi.fn());
    page.frames[0]();
    expect(page.measurements()).toBe(1);
    vi.advanceTimersByTime(FRAME_FALLBACK_MS * 4);
    expect(page.measurements()).toBe(1);
    stop();
  });

  it("leaves no fallback behind when the step unmounts", () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    const page = pageWithoutFrames();
    observe(vi.fn())();
    vi.advanceTimersByTime(FRAME_FALLBACK_MS * 4);
    expect(page.measurements()).toBe(0);
  });
});
