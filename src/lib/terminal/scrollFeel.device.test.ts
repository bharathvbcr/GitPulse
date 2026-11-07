import { afterEach, describe, expect, it, vi } from "vitest";
import {
  GESTURE_GAP_MS,
  SMOOTH_SCROLL_MS,
  WHEEL_LOG_LIMIT,
  createGestureClassifier,
  createScrollFeel,
  createWheelLog,
  glideDuration,
  isWheelNotch,
  scrollFeelOptions,
  type ScrollGuardTerminal,
  type WheelSample,
} from "./scrollFeel";
import { WHEEL_DEBUG_KEY, WHEEL_LOG_GLOBAL, wheelDiagnostic } from "./wheelDiagnostic";

afterEach(() => vi.unstubAllGlobals());

describe("isWheelNotch", () => {
  it("takes a legacy wheelDeltaY that is a non-zero multiple of 120 for a notch", () => {
    expect(isWheelNotch({ deltaY: -100, deltaMode: 0, wheelDeltaY: 120 })).toBe(true);
    expect(isWheelNotch({ deltaY: -300, deltaMode: 0, wheelDeltaY: 360 })).toBe(true);
  });

  it("takes a line-mode event for a notch", () => {
    expect(isWheelNotch({ deltaY: -3, deltaMode: 1 })).toBe(true);
  });

  it("does not take trackpad deltas for a notch: fractional, not a multiple of a notch, or sub-pixel", () => {
    expect(isWheelNotch({ deltaY: -12.5, deltaMode: 0, wheelDeltaY: 37.5 })).toBe(false);
    expect(isWheelNotch({ deltaY: -13, deltaMode: 0, wheelDeltaY: 39 })).toBe(false);
    expect(isWheelNotch({ deltaY: -0.1, deltaMode: 0, wheelDeltaY: 120 })).toBe(false);
    expect(isWheelNotch({ deltaY: -0.4, deltaMode: 1 })).toBe(false);
    expect(isWheelNotch({ deltaY: 0, deltaMode: 0, wheelDeltaY: 0 })).toBe(false);
    expect(isWheelNotch({ deltaY: Number.NaN })).toBe(false);
  });

  it("falls back to a whole-number deltaY only where the legacy field does not exist", () => {
    expect(isWheelNotch({ deltaY: -100 })).toBe(true);
    expect(isWheelNotch({ deltaY: -12.5 })).toBe(false);
  });
});

describe("the gesture classifier", () => {
  const notch = { deltaY: -100, deltaMode: 0, wheelDeltaY: 120 };
  const finger = { deltaY: -12.5, deltaMode: 0, wheelDeltaY: 37.5 };

  it("decides at the first event after a pause and keeps the class for the gesture", () => {
    const classify = createGestureClassifier();
    expect(classify(notch, 0).gestureNotch).toBe(true);
    expect(classify(notch, 70).gestureNotch).toBe(true);
    expect(classify(finger, 1000).gestureNotch).toBe(false);
  });

  it("lets one non-notch event end the notch class for the rest of the gesture, until a pause", () => {
    const classify = createGestureClassifier();
    classify(notch, 0);
    expect(classify(finger, 10).gestureNotch).toBe(false);
    expect(classify(notch, 20).gestureNotch).toBe(false);
    expect(classify(notch, 20 + GESTURE_GAP_MS + 1).gestureNotch).toBe(true);
  });

  it("reports what the event carried, for the diagnostic", () => {
    const sample = createGestureClassifier()({ ...notch, deltaMode: 0 }, 5);
    expect(sample).toEqual({ at: 5, deltaY: -100, wheelDeltaY: 120, deltaMode: 0, notch: true, gestureNotch: true });
    expect(createGestureClassifier()({ deltaY: -100 }, 0).wheelDeltaY).toBeNull();
  });
});

describe("the wheel log", () => {
  it("keeps the last WHEEL_LOG_LIMIT samples", () => {
    const log = createWheelLog();
    const classify = createGestureClassifier();
    for (let i = 0; i < WHEEL_LOG_LIMIT + 7; i += 1) log.push(classify({ deltaY: -100, wheelDeltaY: 120 }, i));
    expect(log.samples()).toHaveLength(WHEEL_LOG_LIMIT);
    expect(log.samples()[0].at).toBe(7);
  });
});

describe("reduced motion and the setting", () => {
  const media = (matches: boolean) => {
    const listeners = new Set<() => void>();
    const list = {
      matches,
      media: "(prefers-reduced-motion: reduce)",
      addEventListener: (_type: string, fn: () => void) => listeners.add(fn),
      removeEventListener: (_type: string, fn: () => void) => listeners.delete(fn),
    };
    return { list, listeners, flip(next: boolean) { list.matches = next; for (const fn of [...listeners]) fn(); } };
  };

  it("sets the glide to 0 when motion is reduced or the setting is off", () => {
    vi.stubGlobal("matchMedia", () => ({ matches: true }));
    expect(glideDuration(true)).toBe(0);
    expect(scrollFeelOptions(true).smoothScrollDuration).toBe(0);
    vi.unstubAllGlobals();
    expect(glideDuration(true)).toBe(SMOOTH_SCROLL_MS);
    expect(glideDuration(false)).toBe(0);
  });

  function open(options: { smoothScrollDuration?: number }, enabled: () => boolean) {
    const listeners: Array<(event: WheelEvent) => void> = [];
    const element = {
      addEventListener: (_t: string, fn: (event: WheelEvent) => void) => listeners.push(fn),
      removeEventListener: (_t: string, fn: (event: WheelEvent) => void) => { listeners.splice(listeners.indexOf(fn), 1); },
    } as unknown as HTMLElement;
    const moved: number[] = [];
    const terminal: ScrollGuardTerminal = {
      element, options, modes: { mouseTrackingMode: "none" }, buffer: { active: { viewportY: 50, baseY: 50, type: "normal" } },
      scrollLines: (n) => moved.push(n),
    };
    const samples: WheelSample[] = [];
    const feel = createScrollFeel(terminal, { enabled, onSample: (s) => samples.push(s) });
    const send = (event: { deltaY: number; deltaMode?: number; wheelDeltaY?: number }, at: number) => listeners.forEach((fn) => fn({ timeStamp: at, ...event } as WheelEvent));
    return { feel, send, moved, samples, listeners };
  }

  it("follows the preference live and removes its listeners on teardown", () => {
    const query = media(false);
    vi.stubGlobal("matchMedia", () => query.list);
    const options = { smoothScrollDuration: SMOOTH_SCROLL_MS };
    const { feel, listeners } = open(options, () => true);
    expect(query.listeners.size).toBe(1);
    query.flip(true);
    expect(options.smoothScrollDuration).toBe(0);
    query.flip(false);
    expect(options.smoothScrollDuration).toBe(SMOOTH_SCROLL_MS);
    feel.detach();
    expect(query.listeners.size).toBe(0);
    expect(listeners).toHaveLength(0);
  });

  it("applies the setting on refresh and keeps the guard quiet while it is off", () => {
    let on = true;
    const options = { smoothScrollDuration: SMOOTH_SCROLL_MS };
    const { feel, send, moved } = open(options, () => on);
    on = false;
    feel.refresh();
    expect(options.smoothScrollDuration).toBe(0);
    send({ deltaY: -100, wheelDeltaY: 120, deltaMode: 0 }, 0);
    expect(moved).toEqual([]);
    on = true;
    feel.refresh();
    expect(options.smoothScrollDuration).toBe(SMOOTH_SCROLL_MS);
  });
});

describe("the glide follows the device, and the guard follows the glide", () => {
  const notch = { deltaY: -100, deltaMode: 0, wheelDeltaY: 120 };
  const finger = { deltaY: -12.5, deltaMode: 0, wheelDeltaY: 37.5 };

  function rig() {
    const writes: number[] = [];
    let duration: number | undefined = SMOOTH_SCROLL_MS;
    const options = { get smoothScrollDuration() { return duration; }, set smoothScrollDuration(value: number | undefined) { writes.push(value ?? -1); duration = value; } };
    const listeners: Array<(event: WheelEvent) => void> = [];
    const element = { addEventListener: (_t: string, fn: (event: WheelEvent) => void) => listeners.push(fn), removeEventListener: () => {} } as unknown as HTMLElement;
    const moved: number[] = [];
    const buffer = { viewportY: 50, baseY: 50, type: "normal" as const };
    createScrollFeel({ element, options, modes: { mouseTrackingMode: "none" }, buffer: { active: buffer }, scrollLines: (n) => { moved.push(n); buffer.viewportY += n; } });
    return { writes, moved, send: (event: object, at: number) => listeners.forEach((fn) => fn({ timeStamp: at, ...event } as WheelEvent)), get duration() { return duration; } };
  }

  it("never touches the duration or the view for a trackpad beyond switching the glide off once", () => {
    const r = rig();
    for (let at = 0; at < 600; at += 8) r.send(finger, at);
    expect(r.moved).toEqual([]);
    expect(r.writes).toEqual([0]);
    expect(r.duration).toBe(0);
  });

  it("switches the glide on once when the device changes to a wheel, and does not write per event", () => {
    const r = rig();
    r.send(finger, 0);
    expect(r.writes).toEqual([0]);
    for (let at = 1000; at < 1000 + 70 * 20; at += 70) r.send(notch, at);
    // 0 -> (guard: already 0, no toggle) -> 150, then nothing for the other 19 events.
    expect(r.writes).toEqual([0, SMOOTH_SCROLL_MS]);
    expect(r.moved).toEqual([-1]);
    expect(r.duration).toBe(SMOOTH_SCROLL_MS);
  });

  it("a sub-pixel trackpad event at the bottom does not move the view", () => {
    const r = rig();
    r.send({ deltaY: -0.1, deltaMode: 0, wheelDeltaY: -0.3 }, 0);
    expect(r.moved).toEqual([]);
  });
});

describe("the opt-in wheel diagnostic", () => {
  it("does nothing unless the flag is set", () => {
    const target: Record<string, unknown> = {};
    expect(wheelDiagnostic({ storage: { getItem: () => null }, target })).toBeUndefined();
    expect(wheelDiagnostic({ storage: { getItem: () => "0" }, target })).toBeUndefined();
    expect(target[WHEEL_LOG_GLOBAL]).toBeUndefined();
  });

  it("is off when storage is unreadable", () => {
    expect(wheelDiagnostic({ storage: { getItem: () => { throw new Error("denied"); } } })).toBeUndefined();
  });

  it("logs each sample to the debug channel and keeps the last ones for the console", () => {
    const target: Record<string, unknown> = {};
    const lines: unknown[][] = [];
    const reads: string[] = [];
    const sink = wheelDiagnostic({ storage: { getItem: (key) => { reads.push(key); return "1"; } }, target, debug: (...args) => lines.push(args) });
    expect(sink).toBeDefined();
    expect(reads).toEqual([WHEEL_DEBUG_KEY]);
    const classify = createGestureClassifier();
    sink?.(classify({ deltaY: -100, deltaMode: 0, wheelDeltaY: 120 }, 0));
    sink?.(classify({ deltaY: -12.5, deltaMode: 0, wheelDeltaY: 37.5 }, 10));
    expect(lines[0][0]).toBe("[gitpulse-terminal]");
    expect(lines[0][2]).toBe("deltaY=-100 wheelDeltaY=120 deltaMode=0 notch=true gesture=notch (glide)");
    expect(lines[1][2]).toContain("gesture=direct");
    const read = target[WHEEL_LOG_GLOBAL] as () => WheelSample[];
    expect(read().map((s) => s.deltaY)).toEqual([-100, -12.5]);
  });
});
