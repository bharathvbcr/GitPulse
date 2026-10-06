import { describe, expect, it } from "vitest";
import {
  BACKGROUND_REFRESH_MIN_MS,
  BACKGROUND_REFRESH_SPACING_MS,
  WATCHER_REFRESH_DEBOUNCE_MS,
  createWatcherRefreshPolicy,
  type WatcherRefreshPolicy,
} from "./watcherRefresh";

/** A deterministic clock and timer queue the policy runs against. */
function harness(initial: { active?: string; hidden?: boolean } = {}) {
  let now = 1_000_000;
  let nextHandle = 1;
  const timers = new Map<number, { at: number; run: () => void }>();
  const state = { active: initial.active ?? "A", hidden: initial.hidden ?? false };
  const refreshes: Array<{ key: string; at: number }> = [];
  const policy: WatcherRefreshPolicy = createWatcherRefreshPolicy({
    now: () => now,
    setTimer: (run, delayMs) => {
      expect(delayMs).toBeGreaterThanOrEqual(0);
      const handle = nextHandle++;
      timers.set(handle, { at: now + delayMs, run });
      return handle;
    },
    clearTimer: (handle) => {
      timers.delete(handle as number);
    },
    isActive: (key) => key === state.active,
    isHidden: () => state.hidden,
    refresh: (key) => refreshes.push({ key, at: now }),
  });
  /** Advances the clock, firing due timers in time order. */
  const advance = (ms: number) => {
    const end = now + ms;
    for (;;) {
      let next: [number, { at: number; run: () => void }] | undefined;
      for (const item of timers) {
        if (item[1].at <= end && (!next || item[1].at < next[1].at)) next = item;
      }
      if (!next) break;
      timers.delete(next[0]);
      now = next[1].at;
      next[1].run();
    }
    now = end;
  };
  const count = (key: string) => refreshes.filter((r) => r.key === key).length;
  return { policy, state, refreshes, advance, count, timers, at: () => now };
}

describe("watcher refresh policy: the active tab", () => {
  it("keeps the storm debounce: a burst is one trailing refresh", () => {
    const h = harness();
    h.policy.onChange("A");
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS - 1);
    h.policy.onChange("A");
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS - 1);
    expect(h.count("A")).toBe(0);
    h.advance(1);
    expect(h.count("A")).toBe(1);
    h.advance(60_000);
    expect(h.count("A")).toBe(1);
  });

  it("refreshes on every settled event, never throttled to the background floor", () => {
    const h = harness();
    for (let i = 0; i < 10; i += 1) {
      h.policy.onChange("A");
      h.advance(2_000);
    }
    expect(h.count("A")).toBe(10);
  });
});

describe("watcher refresh policy: background tabs", () => {
  it("pays the first event soon and the rest at most once per floor", () => {
    const h = harness();
    h.policy.onChange("B");
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("B")).toBe(1);
    // An agent writing every two seconds for five minutes.
    for (let t = 0; t < 300_000; t += 2_000) {
      h.policy.onChange("B");
      h.advance(2_000);
    }
    h.advance(BACKGROUND_REFRESH_MIN_MS);
    const times = h.refreshes.filter((r) => r.key === "B").map((r) => r.at);
    for (let i = 1; i < times.length; i += 1) {
      expect(times[i] - times[i - 1]).toBeGreaterThanOrEqual(BACKGROUND_REFRESH_MIN_MS);
    }
    // Was 150 refreshes (one per event); now bounded by the floor.
    expect(times.length).toBeLessThanOrEqual(1 + Math.ceil(330_000 / BACKGROUND_REFRESH_MIN_MS));
    // The last event is never dropped: the final refresh follows it.
    expect(times[times.length - 1]).toBeGreaterThanOrEqual(h.at() - BACKGROUND_REFRESH_MIN_MS - 2_000);
  });

  it("counts an activation's hydrate as the latest refresh", () => {
    const h = harness();
    h.state.active = "B";
    h.policy.onActivated("B");
    h.state.active = "A";
    h.policy.onChange("B");
    h.advance(BACKGROUND_REFRESH_MIN_MS - 1);
    expect(h.count("B")).toBe(0);
    h.advance(1);
    expect(h.count("B")).toBe(1);
  });

  it("drops a background refresh made redundant by activating the tab", () => {
    const h = harness();
    h.policy.onActivated("B");
    h.policy.onChange("B");
    expect(h.policy.owed()).toEqual(["B"]);
    h.state.active = "B";
    h.policy.onActivated("B");
    expect(h.policy.owed()).toEqual([]);
    h.advance(10 * BACKGROUND_REFRESH_MIN_MS);
    expect(h.count("B")).toBe(0);
  });

  it("promotes an owed background refresh when its tab becomes active", () => {
    const h = harness();
    h.policy.onActivated("B");
    h.policy.onChange("B");
    // Activated by a path that does not report onActivated (close-neighbour).
    h.state.active = "B";
    h.policy.onChange("B");
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("B")).toBe(1);
  });

  it("spaces background refreshes across tabs, bounded however many are busy", () => {
    const h = harness();
    const keys = Array.from({ length: 24 }, (_, i) => `bg-${i}`);
    for (const key of keys) h.policy.onChange(key);
    h.advance(10 * 60_000);
    const times = h.refreshes.map((r) => r.at).sort((a, b) => a - b);
    expect(times.length).toBe(keys.length);
    for (let i = 1; i < times.length; i += 1) {
      expect(times[i] - times[i - 1]).toBeGreaterThanOrEqual(BACKGROUND_REFRESH_SPACING_MS);
    }
  });

  it("gives a cancelled background refresh's slot back", () => {
    const h = harness();
    const keys = Array.from({ length: 10 }, (_, i) => `bg-${i}`);
    for (const key of keys) h.policy.onChange(key);
    // Every armed refresh is cancelled: activated in turn, or closed.
    for (const key of keys.slice(0, 5)) h.policy.onActivated(key);
    for (const key of keys.slice(5)) h.policy.forget(key);
    h.policy.onChange("late");
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    // A running reservation would have parked this behind ten dead slots.
    expect(h.count("late")).toBe(1);
    // Spacing still holds against the refresh that actually ran.
    h.policy.onChange("later");
    h.advance(BACKGROUND_REFRESH_SPACING_MS - 1);
    expect(h.count("later")).toBe(0);
    h.advance(1 + WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("later")).toBe(1);
  });

  it("never lets background spacing delay the active tab", () => {
    const h = harness();
    for (let i = 0; i < 24; i += 1) h.policy.onChange(`bg-${i}`);
    h.policy.onChange("A");
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("A")).toBe(1);
  });

  it("forgets a closed tab: its armed refresh never runs", () => {
    const h = harness();
    h.policy.onChange("B");
    h.policy.forget("B");
    expect(h.timers.size).toBe(0);
    h.advance(BACKGROUND_REFRESH_MIN_MS * 4);
    expect(h.count("B")).toBe(0);
    expect(h.timers.size).toBe(0);
  });
});

describe("watcher refresh policy: hidden window", () => {
  it("spends nothing while hidden and pays each tab once when shown", () => {
    const h = harness({ hidden: true });
    for (let t = 0; t < 600; t += 1) {
      h.policy.onChange("A");
      h.policy.onChange("B");
      h.policy.onChange("C");
      h.advance(1_000);
    }
    expect(h.refreshes).toEqual([]);
    expect(h.timers.size).toBe(0);
    h.state.hidden = false;
    h.policy.onVisibilityChange();
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("A")).toBe(1);
    h.advance(BACKGROUND_REFRESH_SPACING_MS * 3);
    expect(h.count("B")).toBe(1);
    expect(h.count("C")).toBe(1);
    h.advance(10 * BACKGROUND_REFRESH_MIN_MS);
    expect(h.refreshes.length).toBe(3);
  });

  it("serves the active tab first when shown even if it was owed last", () => {
    const h = harness({ hidden: true });
    for (let i = 0; i < 12; i += 1) h.policy.onChange(`bg-${i}`);
    h.policy.onChange("A");
    h.state.hidden = false;
    h.policy.onVisibilityChange();
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.refreshes[0]).toEqual({ key: "A", at: h.at() });
  });

  it("re-checks at fire time: a window hidden after arming owes, not runs", () => {
    const h = harness();
    h.policy.onChange("A");
    h.state.hidden = true;
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("A")).toBe(0);
    expect(h.policy.owed()).toEqual(["A"]);
    h.state.hidden = false;
    h.policy.onVisibilityChange();
    h.advance(WATCHER_REFRESH_DEBOUNCE_MS);
    expect(h.count("A")).toBe(1);
  });

  it("ignores a visibility change that leaves the window hidden", () => {
    const h = harness({ hidden: true });
    h.policy.onChange("A");
    h.policy.onVisibilityChange();
    h.advance(60_000);
    expect(h.refreshes).toEqual([]);
    expect(h.policy.owed()).toEqual(["A"]);
  });
});

describe("watcher refresh policy: lifecycle and stress", () => {
  it("runs nothing after dispose", () => {
    const h = harness();
    h.policy.onChange("A");
    h.policy.onChange("B");
    h.policy.dispose();
    h.policy.onChange("C");
    h.policy.onVisibilityChange();
    h.advance(10 * BACKGROUND_REFRESH_MIN_MS);
    expect(h.refreshes).toEqual([]);
    expect(h.timers.size).toBe(0);
  });

  it("holds every invariant under a randomized hour of events, switches and hides", () => {
    // Seeded LCG so a failure reproduces.
    let seed = 0x9e3779b9;
    const rand = () => {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      return seed / 2 ** 32;
    };
    for (let round = 0; round < 25; round += 1) {
      const h = harness();
      const keys = Array.from({ length: 2 + Math.floor(rand() * 22) }, (_, i) => `r${i}`);
      h.state.active = keys[0];
      h.policy.onActivated(keys[0]);
      // Per key: when its last background refresh ran, and whether it has
      // been the active tab since (an active debounce armed before a switch
      // may legitimately fire soon after, as a background-time refresh).
      const lastBackgroundAt = new Map<string, number>();
      const wasActiveSince = new Set<string>([keys[0]]);
      let backgroundRuns = 0;
      let handOffs = 0;
      const check = (from: number) => {
        // Active tab and visibility are constant within one advance, since
        // they only change between steps.
        for (const run of h.refreshes.slice(from)) {
          expect(h.state.hidden, `refreshed ${run.key} while hidden`).toBe(false);
          if (run.key === h.state.active) {
            wasActiveSince.add(run.key);
            continue;
          }
          backgroundRuns += 1;
          const previous = lastBackgroundAt.get(run.key);
          if (wasActiveSince.has(run.key)) handOffs += 1;
          else if (previous !== undefined) {
            expect(run.at - previous, `${run.key} refreshed under the floor`).toBeGreaterThanOrEqual(
              BACKGROUND_REFRESH_MIN_MS,
            );
          }
          lastBackgroundAt.set(run.key, run.at);
          wasActiveSince.delete(run.key);
        }
      };
      for (let step = 0; step < 1_800; step += 1) {
        const roll = rand();
        const key = keys[Math.floor(rand() * keys.length)];
        if (roll < 0.7) {
          h.policy.onChange(key);
        } else if (roll < 0.78) {
          h.state.active = key;
          h.policy.onActivated(key);
          wasActiveSince.add(key);
        } else if (roll < 0.82) {
          h.state.hidden = !h.state.hidden;
          h.policy.onVisibilityChange();
        }
        const before = h.refreshes.length;
        h.advance(Math.floor(rand() * 4_000));
        check(before);
      }
      // Settle: show the window and let every owed refresh land.
      h.state.hidden = false;
      h.policy.onVisibilityChange();
      const before = h.refreshes.length;
      h.advance(keys.length * BACKGROUND_REFRESH_SPACING_MS + 2 * BACKGROUND_REFRESH_MIN_MS);
      check(before);
      // No event is lost: nothing is owed once shown and settled.
      expect(h.policy.owed()).toEqual([]);
      // Background cost is bounded by the global spacing whatever the tab
      // count; only hand-offs (an active debounce outliving a switch) can
      // exceed it, and there are at most as many as switches.
      const elapsed = h.at() - 1_000_000;
      expect(backgroundRuns - handOffs).toBeLessThanOrEqual(
        Math.ceil(elapsed / BACKGROUND_REFRESH_SPACING_MS) + 1,
      );
    }
  });
});
