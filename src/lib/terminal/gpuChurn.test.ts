import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mulberry32 } from "../__tests__/prng";
import {
  createGpuRenderer,
  createGpuSlots,
  GPU_CHURN_DAMPING_AT,
  GPU_CHURN_FORGIVE_AT,
  GPU_CHURN_WINDOW_MS,
  GPU_LOSS_BUDGET,
  GPU_RECOVER_DELAY_MS,
  type GpuAddon,
  type GpuSlots,
} from "./gpuRenderer";

/**
 * A page that creates WebGL contexts quickly makes its engine force-lose the
 * one drawn least recently (WebKit caps live contexts per page and counts a
 * lost one until it is collected). The pool owns that count: it records every
 * creation, stops releasing hidden holders while the rate is high, and tells
 * a session not to count a loss that the page's own churn caused.
 */

/** A session that holds a slot, and gives it back when released. */
function holder(slots: GpuSlots, name: string, log: string[]) {
  expect(slots.take(), `${name} takes a slot`).toBe(true);
  slots.noteCreated();
  let withdraw: (() => void) | null = null;
  return {
    hide() {
      withdraw = slots.idle(() => {
        log.push(name);
        slots.give();
      });
    },
    show() {
      withdraw?.();
      withdraw = null;
    },
    leave() {
      slots.give();
    },
  };
}

describe("the pool counts the contexts the page creates", () => {
  it("remembers creations for a minute and forgets them after", () => {
    let clock = 1_000;
    const slots = createGpuSlots(2, 8, () => clock);
    expect(slots.recentCreations()).toBe(0);
    slots.noteCreated();
    clock += 30_000;
    slots.noteCreated();
    expect(slots.recentCreations()).toBe(2);
    clock += GPU_CHURN_WINDOW_MS - 30_000;
    expect(slots.recentCreations(), "the first is now a minute old").toBe(1);
    clock += 30_000;
    expect(slots.recentCreations()).toBe(0);
  });

  it("keeps a bounded record however fast creations come", () => {
    const slots = createGpuSlots(2, 8, () => 5);
    for (let n = 0; n < 100_000; n += 1) slots.noteCreated();
    expect(slots.recentCreations()).toBeGreaterThanOrEqual(GPU_CHURN_FORGIVE_AT);
    expect(slots.recentCreations()).toBeLessThanOrEqual(1_024);
  });
});

describe("while the page is creating contexts fast, hidden holders stay", () => {
  it("releases over-budget hidden holders at once below the rate, and stops releasing at it", () => {
    let clock = 0;
    const slots = createGpuSlots(2, 8, () => clock);
    const log: string[] = [];
    holder(slots, "idle", log);
    // One dock shown after another: each show creates, each hide leaves one hidden holder
    // beside the pane on screen, and the next show reclaims it.
    const released: number[] = [];
    let previous: ReturnType<typeof holder> | null = null;
    for (let n = 0; n < GPU_CHURN_DAMPING_AT + 4; n += 1) {
      const dock = holder(slots, `dock${n}`, log);
      previous?.hide();
      previous = dock;
      released.push(log.length);
      clock += 100;
    }
    // Under the rate every show released the previous dock's context; at it, nothing more is
    // released. (The pane on screen's own creation counts, so the rate is reached two rounds early.)
    const settledFrom = GPU_CHURN_DAMPING_AT - 2;
    expect(released[settledFrom], "releases happened while the rate was low").toBeGreaterThan(0);
    const settled = released.slice(settledFrom).every((count) => count === released[settledFrom]);
    expect(settled, `releases by round: ${released.join(",")}`).toBe(true);
    expect(slots.held()).toBeGreaterThan(2);
    expect(slots.held()).toBeLessThanOrEqual(8);
  });

  it("falls back to the tier's budget once the rate drops", () => {
    let clock = 0;
    const slots = createGpuSlots(2, 8, () => clock);
    const log: string[] = [];
    holder(slots, "idle", log);
    const docks = Array.from({ length: GPU_CHURN_DAMPING_AT + 1 }, (_, n) => holder(slots, `dock${n}`, log));
    for (const dock of docks) dock.hide();
    expect(slots.held(), "every hidden holder was kept while creations were frequent").toBe(docks.length + 1);
    clock += GPU_CHURN_WINDOW_MS + 1;
    expect(slots.recentCreations()).toBe(0);
    slots.setCap(2);
    expect(slots.held(), "the budget applies again: the pane on screen plus one hidden").toBe(2);
    expect(log[0]).toBe("dock0");
  });

  it("never holds more than the window's maximum, however fast the page creates", () => {
    const slots = createGpuSlots(2, 8, () => 0);
    const log: string[] = [];
    holder(slots, "idle", log);
    for (let n = 0; n < 40; n += 1) holder(slots, `d${n}`, log).hide();
    expect(slots.held()).toBeLessThanOrEqual(8);
  });

  it("a hidden holder that comes back on screen while the pool is over budget keeps its context", () => {
    let clock = 0;
    const slots = createGpuSlots(1, 8, () => clock);
    const log: string[] = [];
    holder(slots, "a", log);
    const b = holder(slots, "b", log);
    const c = holder(slots, "c", log);
    for (let n = 0; n < GPU_CHURN_DAMPING_AT; n += 1) slots.noteCreated();
    b.hide();
    c.hide();
    expect(log, "kept: the page is creating fast").toEqual([]);
    expect(slots.held()).toBe(3);
    // b returns to the screen: still the same single slot, nothing released, nothing given twice.
    b.show();
    expect(log).toEqual([]);
    expect(slots.held()).toBe(3);
    // The rate drops; the next change trims to the budget, longest-hidden first, and b is on screen so stays.
    clock += GPU_CHURN_WINDOW_MS + 1;
    slots.setCap(1);
    expect(log).toEqual(["c"]);
    expect(slots.held()).toBe(2);
  });
});

/**
 * The pool as it was at c969be52, before the profile: `createGpuSlots(cap)`
 * with one number. A copy, as the oracle the roomy pool is held to.
 */
function baseSlots(cap: number): GpuSlots {
  let held = 0;
  const idles = new Set<() => void>();
  const waiters = new Set<() => void>();
  let reclaiming = 0;
  let notifying = false;
  const reclaimOldest = () => {
    const [oldest] = idles;
    if (!oldest) return;
    idles.delete(oldest);
    reclaiming += 1;
    try {
      oldest();
    } finally {
      reclaiming -= 1;
    }
  };
  const notify = () => {
    if (notifying) return;
    notifying = true;
    try {
      while (waiters.size > 0 && (held < cap || idles.size > 0)) {
        const [next] = waiters;
        waiters.delete(next);
        next();
      }
    } finally {
      notifying = false;
    }
  };
  return {
    take() {
      if (held >= cap) reclaimOldest();
      if (held >= cap) return false;
      held += 1;
      return true;
    },
    give() {
      if (held > 0) held -= 1;
      if (reclaiming === 0) notify();
    },
    held: () => held,
    cap: () => cap,
    setCap: () => undefined,
    noteCreated: () => undefined,
    recentCreations: () => 0,
    idle(release) {
      idles.add(release);
      notify();
      return () => {
        idles.delete(release);
      };
    },
    wait(retry) {
      waiters.add(retry);
      return () => {
        waiters.delete(retry);
      };
    },
  };
}

/** Drives a pool through a seeded sequence and writes down every observable answer. */
function transcript(slots: GpuSlots, seed: number, steps: number): string[] {
  const next = mulberry32(seed);
  const out: string[] = [];
  type Held = { id: number; offered: (() => void) | null; given: boolean };
  const active: Held[] = [];
  const hidden: Held[] = [];
  let ids = 0;
  const release = (h: Held) => () => {
    out.push(`release ${h.id}`);
    const at = hidden.indexOf(h);
    if (at >= 0) hidden.splice(at, 1);
    h.offered = null;
    h.given = true;
    slots.give();
  };
  for (let step = 0; step < steps; step += 1) {
    const roll = next();
    if (roll < 0.35) {
      const ok = slots.take();
      out.push(`take ${ok}`);
      if (ok) active.push({ id: ids++, offered: null, given: false });
    } else if (roll < 0.55 && active.length) {
      const h = active.splice(Math.floor(next() * active.length), 1)[0];
      hidden.push(h);
      h.offered = slots.idle(release(h));
      out.push(`hide ${h.id}`);
    } else if (roll < 0.7 && hidden.length) {
      const h = hidden.splice(Math.floor(next() * hidden.length), 1)[0];
      h.offered?.();
      h.offered = null;
      active.push(h);
      out.push(`show ${h.id}`);
    } else if (roll < 0.85 && active.length) {
      const h = active.splice(Math.floor(next() * active.length), 1)[0];
      slots.give();
      out.push(`leave ${h.id}`);
    } else if (roll < 0.95) {
      const id = ids++;
      slots.wait(() => {
        const ok = slots.take();
        out.push(`retry ${id} ${ok}`);
        if (ok) active.push({ id, offered: null, given: false });
      });
      out.push(`wait ${id}`);
    }
    out.push(`held ${slots.held()}`);
  }
  return out;
}

describe("the roomy pool decides exactly as the pool that shipped", () => {
  for (const cap of [8, 3, 1]) {
    it(`(${cap}, ${cap}) matches the base pool for a recorded sequence, seed by seed`, () => {
      for (let seed = 1; seed <= 40; seed += 1) {
        const expected = transcript(baseSlots(cap), seed, 400);
        const actual = transcript(createGpuSlots(cap, cap), seed, 400);
        expect(actual, `cap ${cap}, seed ${seed}`).toEqual(expected);
      }
    });
  }
});

/** A renderer on a pool, with an addon per attach. */
function sessions(slots: GpuSlots) {
  const made: { fireLoss: () => void; disposed: number }[] = [];
  const renderer = createGpuRenderer({
    attach: (): GpuAddon => {
      let loss: (() => void) | null = null;
      const addon = {
        disposed: 0,
        fireLoss: () => loss?.(),
        dispose() {
          addon.disposed += 1;
        },
        onContextLoss(listener: () => void) {
          loss = listener;
          return { dispose: () => { loss = null; } };
        },
      };
      made.push(addon);
      return addon;
    },
    slots,
  });
  return { renderer, made };
}

describe("a loss the page's own churn caused is not the session's fault", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("the renderer tells the pool about every context it creates", () => {
    const slots = createGpuSlots(2, 8);
    const { renderer, made } = sessions(slots);
    renderer.sync(true);
    expect(made).toHaveLength(1);
    expect(slots.recentCreations()).toBe(1);
  });

  it("counts losses against the session as before while the page is quiet", () => {
    const slots = createGpuSlots(2, 8);
    const { renderer, made } = sessions(slots);
    renderer.sync(true);
    for (let loss = 0; loss <= GPU_LOSS_BUDGET; loss += 1) {
      made.at(-1)?.fireLoss();
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    }
    expect(renderer.state()).toBe("lost");
  });

  it("does not count them once the page has created a dozen contexts in a minute, and re-attaches", () => {
    const slots = createGpuSlots(2, 8);
    for (let n = 0; n < GPU_CHURN_FORGIVE_AT; n += 1) slots.noteCreated();
    const { renderer, made } = sessions(slots);
    renderer.sync(true);
    for (let loss = 0; loss < GPU_LOSS_BUDGET + 3; loss += 1) {
      made.at(-1)?.fireLoss();
      expect(renderer.state()).toBe("reset");
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
      expect(renderer.state(), `after loss ${loss + 1}`).toBe("webgl");
    }
  });

  it("counts losses again once the burst has aged out, and earlier forgiven ones stay forgiven", () => {
    const slots = createGpuSlots(2, 8);
    for (let n = 0; n < GPU_CHURN_FORGIVE_AT; n += 1) slots.noteCreated();
    const { renderer, made } = sessions(slots);
    renderer.sync(true);
    for (let loss = 0; loss < 3; loss += 1) {
      made.at(-1)?.fireLoss();
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    }
    vi.advanceTimersByTime(GPU_CHURN_WINDOW_MS);
    expect(slots.recentCreations(), "the burst and its recoveries have aged out").toBe(0);
    for (let loss = 0; loss <= GPU_LOSS_BUDGET; loss += 1) {
      made.at(-1)?.fireLoss();
      vi.advanceTimersByTime(GPU_RECOVER_DELAY_MS);
    }
    expect(renderer.state()).toBe("lost");
  });
});
