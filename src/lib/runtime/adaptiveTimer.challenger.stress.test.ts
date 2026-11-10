/**
 * Adversarial challenger harness for the adaptive timer under event-loop lag.
 *
 * The dispatcher cases that used to live here exercised `CentralHeartbeatDispatcher`,
 * which `createAdaptiveTimer` never routed through and which has been removed.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAdaptiveTimer } from "./adaptiveTimer";
import { resetForegroundFocus } from "./foreground";
import { LAG_HARD_MS, noteEventLoopDelay, resetEventLoopDelay } from "./loadCadence";

describe("createAdaptiveTimer adversarial stress tests", () => {
  let doc: EventTarget & {
    hidden: boolean;
    visibilityState: string;
    hasFocus: () => boolean;
  };
  let frame: EventTarget & {
    setTimeout: (handler: TimerHandler, ms?: number) => any;
    clearTimeout: (handle: any) => void;
  };
  let hasFocus = true;

  beforeEach(() => {
    vi.useFakeTimers();
    hasFocus = true;
    doc = Object.assign(new EventTarget(), {
      hidden: false,
      visibilityState: "visible",
      hasFocus: () => hasFocus,
    });
    frame = Object.assign(new EventTarget(), {
      setTimeout: (handler: TimerHandler, ms?: number) => globalThis.setTimeout(handler, ms) as any,
      clearTimeout: (handle: any) => globalThis.clearTimeout(handle),
    });
    vi.stubGlobal("document", doc);
    vi.stubGlobal("window", frame);
  });

  afterEach(() => {
    resetEventLoopDelay();
    resetForegroundFocus();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  describe("Event Loop Lag Throttling & Adaptive Cadence", () => {
    it("stretches timer interval under event loop lag conditions", () => {
      let ticks = 0;
      noteEventLoopDelay(LAG_HARD_MS); // 1000ms lag

      // createAdaptiveTimer stretches cadence under hard lag
      const dispose = createAdaptiveTimer(() => ticks++, 1000);

      // Under 1000ms hard lag, base 1000ms delay stretches to 4000ms
      vi.advanceTimersByTime(1000);
      expect(ticks, "Must not tick at 1000ms when hard lag is detected").toBe(0);

      vi.advanceTimersByTime(3000);
      expect(ticks, "Ticks after stretched 4000ms backoff interval").toBe(1);

      dispose();
    });
  });
});
