import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CUE_BAND_PX,
  cueYield,
  EMPTY_OVERFLOW_HINT,
  NO_CUE_YIELD,
  applyHorizontalScrollDelta,
  bindHorizontalWheel,
  observeCueYield,
  observeOverflow,
  OVERFLOW_ROUNDING_SLACK,
  overflowFadeToken,
  overflowNudge,
  readOverflowHint,
  readOverflowInsets,
  resolveOverflowHint,
  scrollChildIntoHorizontalView,
  scrollOverflowBy,
  setOverflowFade,
  verticalWheelToHorizontalDelta,
  WHEEL_LINE_PX,
  wheelDeltaPixels,
  type OverflowBox,
  type OverflowScroller,
} from "./overflowHint";

describe("resolveOverflowHint", () => {
  it("advertises only the side that still has hidden content", () => {
    expect(resolveOverflowHint(0, 440, 1_200)).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
    expect(resolveOverflowHint(400, 440, 1_200)).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: true,
    });
    expect(resolveOverflowHint(760, 440, 1_200)).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: false,
    });
    expect(resolveOverflowHint(0, 440, 440)).toEqual(EMPTY_OVERFLOW_HINT);
  });

  it("still shows an end cue for a sub-2px overflow that canScroll already admits", () => {
    // max = 0.6 is above the 0.5 eps, but a 1px edge threshold would suppress
    // both cues (0 < 0.6-1 is false). The threshold shrinks so the overflow
    // is not silent.
    expect(resolveOverflowHint(0, 100, 100.6)).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
  });

  it("fails closed on non-finite measurements", () => {
    expect(resolveOverflowHint(Number.NaN, 440, 1_200)).toMatchObject({
      canScroll: true,
      showStart: false,
    });
    // A NaN viewport degrades to 0, so content still overflows rather than
    // leaking NaN into the cue. Infinite content is not a number we can pan.
    expect(resolveOverflowHint(0, Number.NaN, 1_200)).toMatchObject({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
    expect(resolveOverflowHint(0, 440, Number.POSITIVE_INFINITY)).toEqual(
      EMPTY_OVERFLOW_HINT,
    );
    expect(resolveOverflowHint(-12, 440, 1_200).showStart).toBe(false);
  });
});

describe("readOverflowHint", () => {
  const box = (partial: Partial<OverflowBox>): OverflowBox => ({
    scrollLeft: 0,
    scrollTop: 0,
    clientWidth: 100,
    clientHeight: 80,
    scrollWidth: 100,
    scrollHeight: 80,
    ...partial,
  });

  it("reads the requested axis", () => {
    const el = box({
      scrollLeft: 20,
      clientWidth: 100,
      scrollWidth: 300,
      scrollTop: 0,
      clientHeight: 80,
      scrollHeight: 400,
    });
    expect(readOverflowHint(el, "x")).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: true,
    });
    expect(readOverflowHint(el, "y")).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
  });
});

describe("overflowNudge / scrollOverflowBy", () => {
  it("pans by 80% of the viewport, never less than 48px", () => {
    expect(overflowNudge(200, "end")).toBe(160);
    expect(overflowNudge(200, "start")).toBe(-160);
    expect(overflowNudge(40, "end")).toBe(48);
    expect(overflowNudge(Number.NaN, "end")).toBe(48);
  });

  it("scrolls only the requested axis", () => {
    const scrollBy = vi.fn();
    const el: OverflowScroller = {
      scrollLeft: 0,
      scrollTop: 0,
      clientWidth: 100,
      clientHeight: 80,
      scrollWidth: 400,
      scrollHeight: 400,
      scrollBy,
    };
    scrollOverflowBy(el, "x", "end", { behavior: "smooth" });
    expect(scrollBy).toHaveBeenCalledWith({
      left: 80,
      top: 0,
      behavior: "smooth",
    });
    scrollBy.mockClear();
    scrollOverflowBy(el, "y", "start");
    expect(scrollBy).toHaveBeenCalledWith({
      left: 0,
      top: -64,
      behavior: "auto",
    });
  });
});

describe("verticalWheelToHorizontalDelta", () => {
  it("maps a dominant vertical wheel onto X and leaves a horizontal one alone", () => {
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: 40 })).toBe(40);
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: -12 })).toBe(-12);
    expect(verticalWheelToHorizontalDelta({ deltaX: 30, deltaY: 10 })).toBeNull();
    expect(verticalWheelToHorizontalDelta({ deltaX: 8, deltaY: 8 })).toBeNull();
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: 0 })).toBeNull();
  });

  it("fails closed on non-finite deltas", () => {
    expect(verticalWheelToHorizontalDelta({ deltaX: Number.NaN, deltaY: 10 })).toBeNull();
    expect(verticalWheelToHorizontalDelta({ deltaX: 0, deltaY: Number.POSITIVE_INFINITY })).toBeNull();
  });
});

describe("applyHorizontalScrollDelta", () => {
  const scroller = (partial: Partial<{ scrollLeft: number; scrollWidth: number; clientWidth: number }>) => ({
    scrollLeft: 0,
    scrollWidth: 800,
    clientWidth: 200,
    ...partial,
  });

  it("clamps to the scrollable range", () => {
    expect(applyHorizontalScrollDelta(scroller({}), 50)).toBe(50);
    expect(applyHorizontalScrollDelta(scroller({ scrollLeft: 580 }), 50)).toBe(600);
    expect(applyHorizontalScrollDelta(scroller({ scrollLeft: 10 }), -50)).toBe(0);
  });

  it("does not move a strip that does not overflow", () => {
    expect(applyHorizontalScrollDelta(scroller({ scrollWidth: 200, clientWidth: 200 }), 80)).toBe(0);
  });

  it("fails closed on hostile measurements", () => {
    expect(applyHorizontalScrollDelta(scroller({ scrollLeft: Number.NaN }), 40)).toBe(40);
    expect(applyHorizontalScrollDelta(scroller({}), Number.NaN)).toBe(0);
    expect(applyHorizontalScrollDelta(scroller({ scrollWidth: Number.POSITIVE_INFINITY }), 40)).toBe(0);
  });
});

describe("scrollChildIntoHorizontalView", () => {
  it("pans just enough to reveal a child past either edge", () => {
    const strip = { scrollLeft: 0, clientWidth: 200, scrollWidth: 800 };
    expect(scrollChildIntoHorizontalView(strip, { offsetLeft: 0, offsetWidth: 80 })).toBe(0);
    expect(scrollChildIntoHorizontalView(strip, { offsetLeft: 250, offsetWidth: 80 })).toBe(130);
    expect(scrollChildIntoHorizontalView({ ...strip, scrollLeft: 400 }, { offsetLeft: 0, offsetWidth: 80 })).toBe(0);
  });

  it("does not overscroll a strip that is already showing the child", () => {
    const strip = { scrollLeft: 100, clientWidth: 200, scrollWidth: 800 };
    expect(scrollChildIntoHorizontalView(strip, { offsetLeft: 120, offsetWidth: 40 })).toBe(100);
  });

  it("fails closed on hostile geometry", () => {
    expect(
      scrollChildIntoHorizontalView(
        { scrollLeft: Number.NaN, clientWidth: 0, scrollWidth: 800 },
        { offsetLeft: 400, offsetWidth: 80 },
      ),
    ).toBe(0);
  });
});

describe("overflowFadeToken", () => {
  const hint = (showStart: boolean, showEnd: boolean) => ({ canScroll: true, showStart, showEnd });

  it("names the axis and every edge that still hides content", () => {
    expect(overflowFadeToken("x", hint(false, true))).toBe("x-end");
    expect(overflowFadeToken("x", hint(true, false))).toBe("x-start");
    expect(overflowFadeToken("x", hint(true, true))).toBe("x-both");
    expect(overflowFadeToken("y", hint(false, true))).toBe("y-end");
    expect(overflowFadeToken("y", hint(true, false))).toBe("y-start");
    expect(overflowFadeToken("y", hint(true, true))).toBe("y-both");
  });

  it("fades nothing when nothing is hidden", () => {
    expect(overflowFadeToken("x", EMPTY_OVERFLOW_HINT)).toBeNull();
    expect(overflowFadeToken("y", hint(false, false))).toBeNull();
  });

  it("never fades a scroller that cannot scroll, whatever the edge flags say", () => {
    // A stale or hand-built hint must not leave a mask on a strip that fits.
    expect(overflowFadeToken("x", { canScroll: false, showStart: true, showEnd: true })).toBeNull();
  });
});

describe("setOverflowFade", () => {
  it("writes the token, replaces it, and clears it", () => {
    const el = { dataset: {} as DOMStringMap };
    setOverflowFade(el, "x-end");
    expect(el.dataset.gpFade).toBe("x-end");
    setOverflowFade(el, "x-both");
    expect(el.dataset.gpFade).toBe("x-both");
    setOverflowFade(el, null);
    expect("gpFade" in el.dataset).toBe(false);
  });

  it("is idempotent, so a repeated hint does not touch the DOM", () => {
    const writes: string[] = [];
    const dataset = new Proxy({} as DOMStringMap, {
      set(target, key, value) { writes.push(`set ${String(key)}`); return Reflect.set(target, key, value); },
      deleteProperty(target, key) { writes.push(`delete ${String(key)}`); return Reflect.deleteProperty(target, key); },
    });
    const el = { dataset };
    setOverflowFade(el, "y-start");
    setOverflowFade(el, "y-start");
    setOverflowFade(el, null);
    setOverflowFade(el, null);
    expect(writes).toEqual(["set gpFade", "delete gpFade"]);
  });
});

describe("bindHorizontalWheel", () => {
  function strip(scrollWidth: number, clientWidth: number) {
    let listener: ((event: WheelEvent) => void) | null = null;
    let options: AddEventListenerOptions | undefined;
    const el = {
      scrollLeft: 0,
      scrollWidth,
      clientWidth,
      addEventListener: vi.fn((_type: "wheel", fn: (event: WheelEvent) => void, opts: AddEventListenerOptions) => {
        listener = fn;
        options = opts;
      }),
      removeEventListener: vi.fn((_type: "wheel", fn: (event: WheelEvent) => void) => {
        if (fn === listener) listener = null;
      }),
    };
    const wheel = (deltaX: number, deltaY: number) => {
      const event = { deltaX, deltaY, preventDefault: vi.fn() } as unknown as WheelEvent;
      listener?.(event);
      return event.preventDefault as ReturnType<typeof vi.fn>;
    };
    return { el, wheel, options: () => options, bound: () => listener !== null };
  }

  it("pans an overflowing strip with a vertical wheel and claims the event", () => {
    const s = strip(1_000, 400);
    bindHorizontalWheel(s.el);
    expect(s.options()).toEqual({ passive: false });
    const prevent = s.wheel(0, 120);
    expect(s.el.scrollLeft).toBe(120);
    expect(prevent).toHaveBeenCalledOnce();
  });

  it("clamps at both ends", () => {
    const s = strip(1_000, 400);
    bindHorizontalWheel(s.el);
    s.wheel(0, 10_000);
    expect(s.el.scrollLeft).toBe(600);
    s.wheel(0, -10_000);
    expect(s.el.scrollLeft).toBe(0);
  });

  it("leaves the wheel to the page when the strip fits or the wheel is already horizontal", () => {
    const fits = strip(400, 400);
    bindHorizontalWheel(fits.el);
    expect(fits.wheel(0, 120)).not.toHaveBeenCalled();
    expect(fits.el.scrollLeft).toBe(0);

    const wide = strip(1_000, 400);
    bindHorizontalWheel(wide.el);
    expect(wide.wheel(80, 10)).not.toHaveBeenCalled();
    expect(wide.wheel(Number.NaN, 120)).not.toHaveBeenCalled();
    expect(wide.el.scrollLeft).toBe(0);
  });

  it("unbinds the same listener it bound", () => {
    const s = strip(1_000, 400);
    const stop = bindHorizontalWheel(s.el);
    expect(s.bound()).toBe(true);
    stop();
    expect(s.bound()).toBe(false);
  });
});

describe("resolveOverflowHint with insets", () => {
  it("does not advertise an edge that hides only padding", () => {
    // 24px of overflow, all of it the toolbar's own 12px padding at each end.
    expect(resolveOverflowHint(0, 266, 290, { start: 12, end: 12 })).toEqual({
      canScroll: true,
      showStart: false,
      showEnd: true,
    });
    expect(resolveOverflowHint(0, 278, 290, { start: 12, end: 12 })).toEqual(EMPTY_OVERFLOW_HINT);
    expect(resolveOverflowHint(12, 278, 290, { start: 12, end: 12 })).toEqual(EMPTY_OVERFLOW_HINT);
  });

  it("counts padding only at the edge it sits on", () => {
    // Scrolled to 0, the start padding is in view: a control hidden past the
    // end is still hidden even though the controls alone would fit.
    const hint = resolveOverflowHint(0, 266, 290, { start: 12, end: 12 });
    expect(hint.showEnd).toBe(true);
    expect(resolveOverflowHint(24, 266, 290, { start: 12, end: 12 })).toEqual({
      canScroll: true,
      showStart: true,
      showEnd: false,
    });
  });

  it("matches the inset-free answer when insets are zero", () => {
    for (const [pos, view, size] of [[0, 440, 1_200], [400, 440, 1_200], [760, 440, 1_200], [0, 100, 100.4]]) {
      expect(resolveOverflowHint(pos, view, size, { start: 0, end: 0 })).toEqual(resolveOverflowHint(pos, view, size));
    }
  });

  it("treats hostile insets as zero rather than inventing or hiding overflow", () => {
    const plain = resolveOverflowHint(100, 400, 1_000);
    for (const bad of [Number.NaN, -50, Number.NEGATIVE_INFINITY]) {
      expect(resolveOverflowHint(100, 400, 1_000, { start: bad, end: bad })).toEqual(plain);
    }
    expect(resolveOverflowHint(100, 400, 1_000, { start: Number.POSITIVE_INFINITY, end: 0 })).toEqual(
      resolveOverflowHint(100, 400, 1_000, { start: 0, end: 0 }),
    );
  });

  it("never shows an edge whose hidden extent is within its inset (randomized)", () => {
    let seed = 7;
    const rand = () => ((seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648) / 2_147_483_648);
    for (let i = 0; i < 5_000; i += 1) {
      const view = rand() * 800;
      const size = view + rand() * 400;
      const max = size - view;
      const pos = rand() * max;
      const insets = { start: rand() * 40, end: rand() * 40 };
      const hint = resolveOverflowHint(pos, view, size, insets);
      if (hint.showStart) expect(pos - insets.start).toBeGreaterThan(0);
      if (hint.showEnd) expect(max - pos - insets.end).toBeGreaterThan(0);
      expect(hint.canScroll).toBe(hint.showStart || hint.showEnd);
      expect(overflowFadeToken("x", hint) === null).toBe(!hint.showStart && !hint.showEnd);
    }
  });
});

describe("readOverflowInsets", () => {
  type Style = Record<string, string>;
  const style = (overrides: Style = {}): Style => ({
    paddingLeft: "0px", paddingRight: "0px", paddingTop: "0px", paddingBottom: "0px",
    borderLeftWidth: "0px", borderRightWidth: "0px", borderTopWidth: "0px", borderBottomWidth: "0px",
    ...overrides,
  });
  const node = (childCount: number, first: object | null = null) => ({ childElementCount: childCount, firstElementChild: first }) as unknown as Element;

  it("adds a lone row's padding and border to the scroller's own padding", () => {
    const row = {};
    const scroller = node(1, row);
    const styles = new Map<object, Style>([
      [scroller, style({ paddingLeft: "4px", paddingRight: "6px" })],
      [row, style({ paddingLeft: "12px", paddingRight: "12px", borderRightWidth: "1px" })],
    ]);
    const insets = readOverflowInsets(scroller, "x", (el) => styles.get(el) as unknown as CSSStyleDeclaration);
    expect(insets).toEqual({ start: 4 + 12 + OVERFLOW_ROUNDING_SLACK, end: 6 + 12 + 1 + OVERFLOW_ROUNDING_SLACK });
  });

  it("does not unwrap one of several children: its padding is not the content's edge", () => {
    const first = {};
    const scroller = node(3, first);
    const styles = new Map<object, Style>([[scroller, style()], [first, style({ paddingLeft: "40px", paddingRight: "40px" })]]);
    expect(readOverflowInsets(scroller, "x", (el) => styles.get(el) as unknown as CSSStyleDeclaration)).toEqual({
      start: OVERFLOW_ROUNDING_SLACK,
      end: OVERFLOW_ROUNDING_SLACK,
    });
  });

  it("reads the block axis for a vertical scroller and ignores garbage values", () => {
    const row = {};
    const scroller = node(1, row);
    const styles = new Map<object, Style>([
      [scroller, style({ paddingTop: "8px", paddingLeft: "99px", paddingBottom: "auto" })],
      [row, style({ paddingBottom: "-3px", borderTopWidth: "2px" })],
    ]);
    expect(readOverflowInsets(scroller, "y", (el) => styles.get(el) as unknown as CSSStyleDeclaration)).toEqual({
      start: 8 + 2 + OVERFLOW_ROUNDING_SLACK,
      end: OVERFLOW_ROUNDING_SLACK,
    });
  });
});

describe("wheelDeltaPixels", () => {
  it("converts line and page wheels to pixels and passes pixels through", () => {
    expect(wheelDeltaPixels(120, 0, 400)).toBe(120);
    expect(wheelDeltaPixels(3, 1, 400)).toBe(3 * WHEEL_LINE_PX);
    expect(wheelDeltaPixels(-1, 2, 400)).toBe(-360);
    // A collapsed strip still pages by at least a line.
    expect(wheelDeltaPixels(1, 2, 0)).toBe(WHEEL_LINE_PX);
  });

  it("turns a non-finite delta into no movement", () => {
    expect(wheelDeltaPixels(Number.NaN, 0, 400)).toBe(0);
    expect(wheelDeltaPixels(Number.POSITIVE_INFINITY, 1, 400)).toBe(0);
  });
});

describe("bindHorizontalWheel leaves the wheel alone when it should", () => {
  function bound(scrollLeft = 0) {
    let listener: ((event: WheelEvent) => void) | null = null;
    const el = {
      scrollLeft, scrollWidth: 1_000, clientWidth: 400,
      addEventListener: (_t: "wheel", fn: (event: WheelEvent) => void) => { listener = fn; },
      removeEventListener: () => { listener = null; },
    };
    bindHorizontalWheel(el);
    const wheel = (init: Partial<WheelEvent>) => {
      const event = { deltaX: 0, deltaY: 0, deltaMode: 0, ctrlKey: false, metaKey: false, defaultPrevented: false, preventDefault: vi.fn(), ...init };
      listener?.(event as unknown as WheelEvent);
      return event.preventDefault;
    };
    return { el, wheel };
  }

  it("lets a pinch or ctrl/cmd+wheel zoom through", () => {
    const { el, wheel } = bound();
    expect(wheel({ deltaY: 120, ctrlKey: true })).not.toHaveBeenCalled();
    expect(wheel({ deltaY: 120, metaKey: true })).not.toHaveBeenCalled();
    expect(el.scrollLeft).toBe(0);
  });

  it("does not handle an event a nested handler already claimed", () => {
    const { el, wheel } = bound();
    expect(wheel({ deltaY: 120, defaultPrevented: true })).not.toHaveBeenCalled();
    expect(el.scrollLeft).toBe(0);
  });

  it("releases the wheel at the end it points past, so scrolling chains on", () => {
    const atEnd = bound(600);
    expect(atEnd.wheel({ deltaY: 120 })).not.toHaveBeenCalled();
    expect(atEnd.wheel({ deltaY: -120 })).toHaveBeenCalledOnce();
    expect(atEnd.el.scrollLeft).toBe(480);
    const atStart = bound(0);
    expect(atStart.wheel({ deltaY: -120 })).not.toHaveBeenCalled();
  });

  it("moves a line-mode wheel by lines", () => {
    const { el, wheel } = bound();
    expect(wheel({ deltaY: 3, deltaMode: 1 })).toHaveBeenCalledOnce();
    expect(el.scrollLeft).toBe(3 * WHEEL_LINE_PX);
  });
});

describe("observeOverflow", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function harness(childCount: number) {
    const frames = new Map<number, FrameRequestCallback>();
    let nextFrame = 0;
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      nextFrame += 1;
      frames.set(nextFrame, cb);
      return nextFrame;
    });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
    vi.stubGlobal("getComputedStyle", () => ({ paddingLeft: "0px", paddingRight: "0px", paddingTop: "0px", paddingBottom: "0px", borderLeftWidth: "0px", borderRightWidth: "0px", borderTopWidth: "0px", borderBottomWidth: "0px" }));
    let mutate: (() => void) | null = null;
    let moConnected = false;
    vi.stubGlobal("MutationObserver", class {
      constructor(cb: () => void) { mutate = cb; }
      observe() { moConnected = true; }
      disconnect() { moConnected = false; }
    });
    const held = new Map<Element, ResizeObserverOptions | undefined>();
    let resized: ResizeObserverCallback | null = null;
    class FakeRO {
      constructor(cb: ResizeObserverCallback) { resized = cb; }
      observe(target: Element, options?: ResizeObserverOptions) { held.set(target, options); }
      unobserve(target: Element) { held.delete(target); }
      disconnect() { held.clear(); }
    }
    const kid = () => ({}) as Element;
    const el = {
      children: Array.from({ length: childCount }, kid),
      get childElementCount() { return this.children.length; },
      get firstElementChild() { return this.children[0] ?? null; },
      scrollLeft: 0, scrollTop: 0, clientWidth: 400, clientHeight: 40, scrollWidth: 1_000, scrollHeight: 40,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    };
    const hints: unknown[] = [];
    const stop = observeOverflow(el as unknown as HTMLElement, "x", (hint) => hints.push(hint), {
      ResizeObserver: FakeRO as unknown as new (cb: ResizeObserverCallback) => ResizeObserver,
      raf: (cb) => { cb(0); return 1; },
      caf: () => undefined,
    });
    return {
      el, hints, held, stop, kid,
      mutate: () => mutate?.(),
      resize: () => resized?.([{ target: el } as unknown as ResizeObserverEntry], {} as ResizeObserver),
      flushFrames: () => {
        const due = [...frames.values()];
        frames.clear();
        due.forEach((cb) => cb(0));
      },
      moConnected: () => moConnected,
    };
  }

  it("answers synchronously, so a retargeted cue never shows the old scroller for a frame", () => {
    const h = harness(3);
    expect(h.hints).toEqual([{ canScroll: true, showStart: false, showEnd: true }]);
  });

  it("observes children by border box and lets go of the ones that leave", () => {
    const h = harness(3);
    const [a, b, c] = h.el.children;
    expect([...h.held.keys()]).toEqual([h.el, a, b, c]);
    expect(h.held.get(a)).toEqual({ box: "border-box" });
    for (let round = 0; round < 500; round += 1) {
      h.el.children = [h.kid(), h.kid()];
      h.mutate();
    }
    expect(h.held.size).toBe(1 + 2);
    expect(h.held.has(a) || h.held.has(b) || h.held.has(c)).toBe(false);
  });

  it("re-reads on resize and on child changes, and coalesces scroll into a frame", () => {
    const h = harness(1);
    h.resize();
    expect(h.hints).toHaveLength(2);
    h.mutate();
    h.mutate();
    // Child changes are coalesced into the next frame, like scroll.
    expect(h.hints).toHaveLength(2);
    h.flushFrames();
    expect(h.hints).toHaveLength(3);
    const onScroll = h.el.addEventListener.mock.calls[0]?.[1] as () => void;
    h.el.scrollLeft = 600;
    onScroll();
    onScroll();
    expect(h.hints).toHaveLength(3);
    h.flushFrames();
    expect(h.hints).toHaveLength(4);
    expect(h.hints.at(-1)).toEqual({ canScroll: true, showStart: true, showEnd: false });
  });

  it("says nothing after it is stopped, whatever fires late", () => {
    const h = harness(2);
    const onScroll = h.el.addEventListener.mock.calls[0]?.[1] as () => void;
    onScroll();
    h.stop();
    const before = h.hints.length;
    h.mutate();
    h.resize();
    h.flushFrames();
    expect(h.hints).toHaveLength(before);
    expect(h.held.size).toBe(0);
    expect(h.moConnected()).toBe(false);
    expect(h.el.removeEventListener).toHaveBeenCalledWith("scroll", onScroll);
  });
});

describe("cueYield", () => {
  // The sidebar case: the branch list is h-full inside the sidebar body, so at
  // rest its bottom edge sits the body's 12px padding above the body's own.
  const body = { start: 100, end: 800 };
  const list = { start: 140, end: 788 };

  it("gives way at an edge where the enclosing scroller draws its own chevron", () => {
    expect(cueYield(list, body, "y-end", "y")).toEqual({ start: false, end: true });
    expect(cueYield(list, body, "y-both", "y")).toEqual({ start: false, end: true });
  });

  it("keeps its chevron when the enclosing scroller draws none at that edge", () => {
    expect(cueYield(list, body, null, "y")).toEqual({ start: false, end: false });
    expect(cueYield(list, body, undefined, "y")).toEqual({ start: false, end: false });
    expect(cueYield(list, body, "y-start", "y")).toEqual({ start: false, end: false });
  });

  it("keeps its chevron once its edge is clear of the enclosing chevron's band", () => {
    // Scrolled so the list ends well above the body's bottom: two edges, two cues.
    const clear = { start: 20, end: body.end - CUE_BAND_PX - 1 };
    expect(cueYield(clear, body, "y-end", "y").end).toBe(false);
    const touching = { start: 20, end: body.end - CUE_BAND_PX + 1 };
    expect(cueYield(touching, body, "y-end", "y").end).toBe(true);
  });

  it("applies the same rule to the start edge", () => {
    const top = { start: body.start + 4, end: 600 };
    expect(cueYield(top, body, "y-start", "y")).toEqual({ start: true, end: false });
    expect(cueYield({ start: body.start + CUE_BAND_PX + 1, end: 600 }, body, "y-both", "y").start).toBe(false);
  });

  it("never reads another axis's token as its own", () => {
    expect(cueYield(list, body, "x-both", "y")).toEqual({ start: false, end: false });
    expect(cueYield({ start: 0, end: 300 }, { start: 0, end: 310 }, "y-end", "x").end).toBe(false);
  });
});

describe("observeCueYield", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  type Rect = { left: number; right: number; top: number; bottom: number };
  type Node = {
    parentElement: Node | null;
    dataset: Record<string, string | undefined>;
    rect: Rect;
    overflow: { x: string; y: string };
    listeners: Map<string, () => void>;
    getBoundingClientRect: () => Rect;
    addEventListener: (type: string, fn: () => void) => void;
    removeEventListener: (type: string, fn: () => void) => void;
  };

  function node(parent: Node | null, rect: Rect, overflow = { x: "visible", y: "visible" }): Node {
    const n: Node = {
      parentElement: parent,
      dataset: {},
      rect,
      overflow,
      listeners: new Map(),
      getBoundingClientRect: () => n.rect,
      addEventListener: (type, fn) => n.listeners.set(type, fn),
      removeEventListener: (type, fn) => {
        if (n.listeners.get(type) === fn) n.listeners.delete(type);
      },
    };
    return n;
  }

  function harness(axis: "x" | "y", outerOverflow: { x: string; y: string }) {
    const frames = new Map<number, FrameRequestCallback>();
    let nextFrame = 0;
    vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
      nextFrame += 1;
      frames.set(nextFrame, cb);
      return nextFrame;
    });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
    vi.stubGlobal("getComputedStyle", (n: Node) => ({ overflowX: n.overflow.x, overflowY: n.overflow.y }));
    let mutate: (() => void) | null = null;
    let fadeFilter: unknown = null;
    let moConnected = false;
    vi.stubGlobal("MutationObserver", class {
      constructor(cb: () => void) { mutate = cb; }
      observe(_target: unknown, options: { attributeFilter?: string[] }) { moConnected = true; fadeFilter = options.attributeFilter; }
      disconnect() { moConnected = false; }
    });
    const held = new Set<unknown>();
    let resized: ResizeObserverCallback | null = null;
    class FakeRO {
      constructor(cb: ResizeObserverCallback) { resized = cb; }
      observe(target: unknown) { held.add(target); }
      unobserve(target: unknown) { held.delete(target); }
      disconnect() { held.clear(); }
    }
    // The sidebar shape: a scrolling body, a plain wrapper, then the list.
    const outer = node(null, { left: 0, right: 300, top: 0, bottom: 500 }, outerOverflow);
    const wrapper = node(outer, { left: 0, right: 300, top: 0, bottom: 500 });
    const inner = node(wrapper, { left: 0, right: 300, top: 0, bottom: 500 }, { x: "auto", y: "auto" });
    const answers: unknown[] = [];
    const stop = observeCueYield(inner as unknown as HTMLElement, axis, (next) => answers.push(next), {
      ResizeObserver: FakeRO as unknown as new (cb: ResizeObserverCallback) => ResizeObserver,
      raf: (cb) => { cb(0); return 1; },
      caf: () => undefined,
    });
    return {
      outer, inner, answers, stop, held,
      mutate: () => mutate?.(),
      resize: () => resized?.([{ target: inner } as unknown as ResizeObserverEntry], {} as ResizeObserver),
      flushFrames: () => {
        const due = [...frames.values()];
        frames.clear();
        due.forEach((cb) => cb(0));
      },
      pendingFrames: () => frames.size,
      fadeFilter: () => fadeFilter,
      moConnected: () => moConnected,
    };
  }

  it("answers NO_CUE_YIELD at once when no ancestor scrolls on the axis, and its dispose is harmless", () => {
    const h = harness("y", { x: "auto", y: "visible" });
    expect(h.answers).toEqual([NO_CUE_YIELD]);
    expect(h.held.size).toBe(0);
    expect(h.inner.listeners.size).toBe(0);
    h.stop();
    expect(h.answers).toEqual([NO_CUE_YIELD]);
  });

  it("skips non-scrolling ancestors, answers synchronously, and yields the end chevron the enclosing scroller draws", () => {
    const h = harness("y", { x: "visible", y: "auto" });
    expect(h.answers).toEqual([{ start: false, end: false }]);
    expect([...h.held]).toEqual([h.inner, h.outer]);
    expect(h.fadeFilter()).toEqual(["data-gp-fade"]);
    h.outer.dataset.gpFade = "y-end";
    h.mutate();
    h.mutate();
    expect(h.pendingFrames()).toBe(1);
    h.flushFrames();
    expect(h.answers.at(-1)).toEqual({ start: false, end: true });
  });

  it("re-reads on scroll of either scroller and on resize, on the x axis by left and right", () => {
    const h = harness("x", { x: "scroll", y: "visible" });
    h.outer.dataset.gpFade = "x-both";
    h.inner.rect = { left: 100, right: 200, top: 0, bottom: 500 };
    h.resize();
    expect(h.answers.at(-1)).toEqual({ start: false, end: false });
    h.inner.rect = { left: 10, right: 290, top: 0, bottom: 500 };
    h.inner.listeners.get("scroll")?.();
    h.outer.listeners.get("scroll")?.();
    expect(h.pendingFrames()).toBe(1);
    h.flushFrames();
    expect(h.answers.at(-1)).toEqual({ start: true, end: true });
  });

  it("delivers nothing after dispose and lets go of every listener and observer", () => {
    const h = harness("y", { x: "visible", y: "auto" });
    h.mutate();
    const count = h.answers.length;
    h.stop();
    expect(h.pendingFrames()).toBe(0);
    h.flushFrames();
    h.resize();
    expect(h.answers.length).toBe(count);
    expect(h.inner.listeners.size).toBe(0);
    expect(h.outer.listeners.size).toBe(0);
    expect(h.held.size).toBe(0);
    expect(h.moConnected()).toBe(false);
  });
});
