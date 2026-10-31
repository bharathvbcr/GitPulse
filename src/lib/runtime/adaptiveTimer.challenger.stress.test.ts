/**
 * Adversarial Challenger Stress Harness for Centralized Adaptive Heartbeat Dispatcher (Feature 8).
 *
 * Empirically challenges:
 * 1. Cadence Transitions: active (1s) -> idle (5s) -> background (15s/30s) -> active (1s).
 * 2. User Activity Resets: window keydown/pointerdown resets idle timer back to active.
 * 3. Foreground / Focus Transitions: blur drops timer, focus flushes catch-up tick without timer stacking.
 * 4. High Subscriber Load: 60+ subscribers with varying intervals [1s, 2s, 5s, 30s] executing in batched steps.
 * 5. Coordinated Batching: subscribers due at the same timestamp share identical tickIndex and timestamp.
 * 6. High-Load Fault Isolation: throwing subscribers do not disrupt sibling subscribers or dispatcher integrity.
 * 7. Adaptive Throttling: setThrottled doubles effective intervals deterministically.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  CentralHeartbeatDispatcher,
  createAdaptiveTimer,
  type HeartbeatTick,
} from "./adaptiveTimer";
import { resetForegroundFocus } from "./foreground";
import { LAG_HARD_MS, noteEventLoopDelay, resetEventLoopDelay } from "./loadCadence";

describe("CentralHeartbeatDispatcher Adversarial Stress Tests", () => {
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

  describe("Cadence Transitions Under Visibility, Inactivity & Focus", () => {
    it("transitions active (1s) -> idle (5s) on inactivity, and recovers immediately on user keydown", () => {
      const dispatcher = new CentralHeartbeatDispatcher();

      // Active state initially
      const initial = dispatcher.currentCadenceDelay();
      expect(initial.cadence).toBe("active");
      expect(initial.delayMs).toBe(1_000);

      // Advance time by 16 seconds (inactivity threshold is 15s)
      vi.advanceTimersByTime(16_000);

      // Now should be in idle cadence
      const idle = dispatcher.currentCadenceDelay();
      expect(idle.cadence).toBe("idle");
      expect(idle.delayMs).toBe(5_000);

      // User presses a key
      frame.dispatchEvent(new Event("keydown"));

      // Immediately recovers to active cadence
      const recovered = dispatcher.currentCadenceDelay();
      expect(recovered.cadence).toBe("active");
      expect(recovered.delayMs).toBe(1_000);

      dispatcher.dispose();
    });

    it("transitions to background (30s) when document becomes hidden", () => {
      const dispatcher = new CentralHeartbeatDispatcher();

      doc.hidden = true;
      doc.visibilityState = "hidden";

      const bg = dispatcher.currentCadenceDelay();
      expect(bg.cadence).toBe("background");
      expect(bg.delayMs).toBe(30_000);

      // Unhide document
      doc.hidden = false;
      doc.visibilityState = "visible";

      const restored = dispatcher.currentCadenceDelay();
      expect(restored.cadence).toBe("active");
      expect(restored.delayMs).toBe(1_000);

      dispatcher.dispose();
    });

    it("transitions to background (15s) on window blur or explicit throttled state", () => {
      const dispatcher = new CentralHeartbeatDispatcher();

      // Window blur event
      hasFocus = false;
      frame.dispatchEvent(new Event("blur"));

      const blurred = dispatcher.currentCadenceDelay();
      expect(blurred.cadence).toBe("background");
      expect(blurred.delayMs).toBe(15_000);

      // Window focus event recovers
      hasFocus = true;
      frame.dispatchEvent(new Event("focus"));

      const focused = dispatcher.currentCadenceDelay();
      expect(focused.cadence).toBe("active");
      expect(focused.delayMs).toBe(1_000);

      // Explicit setThrottled(true)
      dispatcher.setThrottled(true);
      const throttled = dispatcher.currentCadenceDelay();
      expect(throttled.cadence).toBe("background");
      expect(throttled.delayMs).toBe(15_000);

      dispatcher.dispose();
    });

    it("flushes a catch-up tick upon returning to foreground without creating duplicate timer handles", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      let subscriberTicks = 0;
      let lastCadence = "";

      dispatcher.subscribe((tick) => {
        subscriberTicks++;
        lastCadence = tick.cadence;
      }, 1000);

      // Active tick runs
      expect(vi.getTimerCount()).toBe(1);

      // Blur window (stops active timer)
      hasFocus = false;
      frame.dispatchEvent(new Event("blur"));
      expect(vi.getTimerCount()).toBe(0);

      // Return focus to window
      hasFocus = true;
      frame.dispatchEvent(new Event("focus"));

      // Subscriber received immediate catch-up tick
      expect(subscriberTicks).toBe(1);
      expect(lastCadence).toBe("active");

      // Exactly 1 timer is scheduled for the next interval (no stacking)
      expect(vi.getTimerCount()).toBe(1);

      dispatcher.dispose();
      expect(vi.getTimerCount()).toBe(0);
    });
  });

  describe("High Subscriber Load (60 Subscribers: [1s, 2s, 5s, 30s])", () => {
    it("batches dispatches deterministically across 60 subscribers with varying intervals", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      const baseTime = Date.now();

      type SubRecord = { id: number; interval: number; ticks: HeartbeatTick[] };
      const records: SubRecord[] = [];

      // Register 60 subscribers:
      // 15 at 1s, 15 at 2s, 15 at 5s, 15 at 30s
      const intervals = [1000, 2000, 5000, 30000];
      let idCounter = 1;
      for (const interval of intervals) {
        for (let i = 0; i < 15; i++) {
          const rec: SubRecord = { id: idCounter++, interval, ticks: [] };
          records.push(rec);
          dispatcher.subscribe((tick) => {
            rec.ticks.push(tick);
          }, interval);
        }
      }

      expect(dispatcher.subscriberCount()).toBe(60);

      // Step at t = baseTime + 1,000ms: only the 15 1s subscribers should fire
      let dispatched = dispatcher.step(baseTime + 1000);
      expect(dispatched).toBe(15);
      expect(records.filter((r) => r.ticks.length === 1).length).toBe(15);
      expect(records.filter((r) => r.interval === 1000).every((r) => r.ticks.length === 1)).toBe(true);
      expect(records.filter((r) => r.interval > 1000).every((r) => r.ticks.length === 0)).toBe(true);

      // Step at t = baseTime + 2,000ms: 1s subscribers fire (2nd time) + 2s subscribers fire (1st time) = 30 total
      dispatched = dispatcher.step(baseTime + 2000);
      expect(dispatched).toBe(30);

      // Verify batching coordination: all 30 subscribers received identical tickIndex and timestamp
      const batch2000Ticks = records
        .filter((r) => r.interval === 1000 || r.interval === 2000)
        .map((r) => r.ticks[r.ticks.length - 1]);
      const tickIndex2000 = batch2000Ticks[0].tickIndex;
      expect(batch2000Ticks.every((t) => t.tickIndex === tickIndex2000)).toBe(true);
      expect(batch2000Ticks.every((t) => t.timestamp === baseTime + 2000)).toBe(true);

      // Step across 60 seconds (virtual time from 3000 to 60000 in 1s steps)
      for (let offset = 3000; offset <= 60000; offset += 1000) {
        dispatcher.step(baseTime + offset);
      }

      // Final count validation across the 60-second test window:
      // - 1s subscribers: 60 ticks each
      // - 2s subscribers: 30 ticks each
      // - 5s subscribers: 12 ticks each (at 5k, 10k, 15k, 20k, 25k, 30k, 35k, 40k, 45k, 50k, 55k, 60k)
      // - 30s subscribers: 2 ticks each (at 30k, 60k)
      for (const rec of records) {
        if (rec.interval === 1000) expect(rec.ticks.length).toBe(60);
        else if (rec.interval === 2000) expect(rec.ticks.length).toBe(30);
        else if (rec.interval === 5000) expect(rec.ticks.length).toBe(12);
        else if (rec.interval === 30000) expect(rec.ticks.length).toBe(2);
      }

      dispatcher.dispose();
    });

    it("fault isolation: 12 failing subscribers throwing errors do not disrupt the remaining 48 subscribers", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      const baseTime = Date.now();

      let healthyTicks = 0;
      let failingAttempts = 0;

      // 12 failing subscribers
      for (let i = 0; i < 12; i++) {
        dispatcher.subscribe(() => {
          failingAttempts++;
          throw new Error("simulated subscriber crash");
        }, 1000);
      }

      // 48 healthy subscribers
      for (let i = 0; i < 48; i++) {
        dispatcher.subscribe(() => {
          healthyTicks++;
        }, 1000);
      }

      // Run 5 steps
      for (let offset = 1000; offset <= 5000; offset += 1000) {
        expect(() => dispatcher.step(baseTime + offset)).not.toThrow();
      }

      // All 48 healthy subscribers executed on every step (48 * 5 = 240)
      expect(healthyTicks).toBe(240);
      // All 12 failing subscribers were attempted on every step (12 * 5 = 60)
      expect(failingAttempts).toBe(60);

      dispatcher.dispose();
    });

    it("adaptive throttling doubles effective subscriber intervals", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      const baseTime = Date.now();

      let sub1000Ticks = 0;
      let sub2000Ticks = 0;

      dispatcher.subscribe(() => sub1000Ticks++, 1000);
      dispatcher.subscribe(() => sub2000Ticks++, 2000);

      // Normal unthrottled: 1s ticks at 1s, 2s ticks at 2s
      dispatcher.step(baseTime + 1000);
      expect(sub1000Ticks).toBe(1);
      expect(sub2000Ticks).toBe(0);

      dispatcher.step(baseTime + 2000);
      expect(sub1000Ticks).toBe(2);
      expect(sub2000Ticks).toBe(1);

      // Now enable throttling: effective intervals become 2s (for 1000ms) and 4s (for 2000ms)
      dispatcher.setThrottled(true);

      // Step at 3000ms: only 1s has elapsed since 2000ms, less than 2000ms throttled interval -> 0 ticks
      dispatcher.step(baseTime + 3000);
      expect(sub1000Ticks).toBe(2);
      expect(sub2000Ticks).toBe(1);

      // Step at 4000ms: 2s elapsed since 2000ms -> sub1000 fires! (sub2000 still waiting for 4s delta)
      dispatcher.step(baseTime + 4000);
      expect(sub1000Ticks).toBe(3);
      expect(sub2000Ticks).toBe(1);

      // Step at 6000ms: 4s elapsed since 2000ms for sub2000 -> both fire
      dispatcher.step(baseTime + 6000);
      expect(sub1000Ticks).toBe(4);
      expect(sub2000Ticks).toBe(2);

      dispatcher.dispose();
    });

    it("dynamic subscriber unregistration during execution cleanly decrements subscriber count", () => {
      const dispatcher = new CentralHeartbeatDispatcher();
      const baseTime = Date.now();

      const unsubs: (() => void)[] = [];
      let ticks = 0;

      for (let i = 0; i < 50; i++) {
        const unsub = dispatcher.subscribe(() => ticks++, 1000);
        unsubs.push(unsub);
      }

      expect(dispatcher.subscriberCount()).toBe(50);
      dispatcher.step(baseTime + 1000);
      expect(ticks).toBe(50);

      // Unregister 25 subscribers
      for (let i = 0; i < 25; i++) {
        unsubs[i]();
      }

      expect(dispatcher.subscriberCount()).toBe(25);
      dispatcher.step(baseTime + 2000);
      // Only the remaining 25 subscribers fired
      expect(ticks).toBe(75);

      dispatcher.dispose();
      expect(dispatcher.subscriberCount()).toBe(0);
    });
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
