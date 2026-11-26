import { describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  createIdleShedder,
  IDLE_MEMORY_SCHEDULES,
  type IdleShedderActions,
  type IdleShedderTimers,
  type IdleStage,
} from "./idleShedder";
import { resetForegroundFocus } from "./foreground";

function fakeClock() {
  let now = 1_000;
  let nextId = 1;
  const tasks = new Map<number, { at: number; cb: () => void }>();
  const timers: IdleShedderTimers = {
    now: () => now,
    setTimeout(cb, ms) {
      const id = nextId++;
      tasks.set(id, { at: now + ms, cb });
      return id;
    },
    clearTimeout(handle) {
      tasks.delete(handle as number);
    },
  };
  const advance = (ms: number) => {
    const target = now + ms;
    while (true) {
      let earliestId: number | null = null;
      let earliestAt = Infinity;
      for (const [id, task] of tasks) {
        if (task.at <= target && task.at < earliestAt) {
          earliestAt = task.at;
          earliestId = id;
        }
      }
      if (earliestId === null) break;
      const task = tasks.get(earliestId)!;
      tasks.delete(earliestId);
      now = task.at;
      task.cb();
    }
    now = target;
  };
  return { timers, advance };
}

function fakeHost() {
  const docListeners = new Map<string, Set<() => void>>();
  const winListeners = new Map<string, Set<() => void>>();
  let hidden = false;
  let focused = true;

  const doc = {
    get hidden() {
      return hidden;
    },
    get visibilityState() {
      return hidden ? "hidden" : "visible";
    },
    hasFocus: () => focused,
    addEventListener(type: string, fn: () => void) {
      if (!docListeners.has(type)) docListeners.set(type, new Set());
      docListeners.get(type)!.add(fn);
    },
    removeEventListener(type: string, fn: () => void) {
      docListeners.get(type)?.delete(fn);
    },
  };

  const win = {
    addEventListener(type: string, fn: () => void) {
      if (!winListeners.has(type)) winListeners.set(type, new Set());
      winListeners.get(type)!.add(fn);
    },
    removeEventListener(type: string, fn: () => void) {
      winListeners.get(type)?.delete(fn);
    },
  };

  const fire = (target: Map<string, Set<() => void>>, type: string) => {
    for (const fn of [...(target.get(type) ?? [])]) fn();
  };

  return {
    doc: doc as unknown as EventTarget & { hidden: boolean; visibilityState: string; hasFocus: () => boolean },
    win: win as unknown as EventTarget,
    blur() {
      focused = false;
      fire(winListeners, "blur");
    },
    focus() {
      focused = true;
      hidden = false;
      fire(winListeners, "focus");
    },
    hide() {
      hidden = true;
      focused = false;
      fire(docListeners, "visibilitychange");
    },
    show() {
      hidden = false;
      focused = true;
      fire(docListeners, "visibilitychange");
      fire(winListeners, "focus");
    },
  };
}

function spyActions(overrides: Partial<IdleShedderActions> = {}) {
  const events: string[] = [];
  const actions: IdleShedderActions = {
    onSoftShed: vi.fn(() => events.push("soft")),
    onDeepShed: vi.fn(() => events.push("deep")),
    onResume: vi.fn((prev: IdleStage) => events.push(`resume:${prev}`)),
    hibernateBlocker: vi.fn(() => null),
    onHibernate: vi.fn(() => {
      events.push("hibernate");
      return true;
    }),
    ...overrides,
  };
  return { actions, events };
}

describe("idleShedder", () => {
  it("never escalates to deep_shed or hibernation when unfocused on a second monitor", () => {
    resetForegroundFocus();
    const { timers, advance } = fakeClock();
    const host = fakeHost();
    const { actions, events } = spyActions();

    const shedder = createIdleShedder(actions, { initialMode: "balanced", doc: host.doc, win: host.win, timers });
    host.blur();
    advance(IDLE_MEMORY_SCHEDULES.balanced.softShedMs - 1);
    expect(get(shedder.stage)).toBe("active");

    advance(1);
    expect(get(shedder.stage)).toBe("soft_shed");

    // Staying unfocused but visible for an hour must never deep-shed or hibernate.
    advance(60 * 60_000);
    expect(get(shedder.stage)).toBe("soft_shed");
    expect(events).toEqual(["soft"]);

    host.focus();
    expect(get(shedder.stage)).toBe("active");
    expect(events).toEqual(["soft", "resume:soft_shed"]);
    shedder.stop();
  });

  it("progresses through soft_shed -> deep_shed -> hibernating when truly hidden", async () => {
    resetForegroundFocus();
    const { timers, advance } = fakeClock();
    const host = fakeHost();
    const { actions, events } = spyActions();

    const shedder = createIdleShedder(actions, { initialMode: "balanced", doc: host.doc, win: host.win, timers });
    host.hide();
    advance(IDLE_MEMORY_SCHEDULES.balanced.deepShedMs);
    expect(get(shedder.stage)).toBe("deep_shed");
    expect(events).toEqual(["soft", "deep"]);

    advance(IDLE_MEMORY_SCHEDULES.balanced.hibernateMs);
    await Promise.resolve();
    expect(get(shedder.stage)).toBe("hibernating");
    expect(events).toEqual(["soft", "deep", "hibernate"]);
    shedder.stop();
  });

  it("defers hibernation when unsaved drafts or active mutations exist and retries later", async () => {
    resetForegroundFocus();
    const { timers, advance } = fakeClock();
    const host = fakeHost();
    let blocker: string | null = "unsaved editor drafts";
    const { actions, events } = spyActions({
      hibernateBlocker: () => blocker,
    });

    const shedder = createIdleShedder(actions, { initialMode: "balanced", doc: host.doc, win: host.win, timers });
    host.hide();
    advance(IDLE_MEMORY_SCHEDULES.balanced.hibernateMs);
    await Promise.resolve();

    expect(get(shedder.stage)).toBe("deep_shed");
    expect(get(shedder.metrics).hibernateDeferrals).toBe(1);
    expect(get(shedder.metrics).lastDeferReason).toBe("unsaved editor drafts");
    expect(events).toEqual(["soft", "deep"]);

    blocker = null;
    advance(IDLE_MEMORY_SCHEDULES.balanced.hibernateMs);
    await Promise.resolve();
    expect(get(shedder.stage)).toBe("hibernating");
    expect(events).toEqual(["soft", "deep", "hibernate"]);
    shedder.stop();
  });

  it("resets timers without shedding on rapid Cmd+Tab focus churn", () => {
    resetForegroundFocus();
    const { timers, advance } = fakeClock();
    const host = fakeHost();
    const { actions, events } = spyActions();

    const shedder = createIdleShedder(actions, { initialMode: "balanced", doc: host.doc, win: host.win, timers });
    for (let i = 0; i < 10; i += 1) {
      host.blur();
      advance(10_000);
      host.focus();
      advance(1_000);
    }
    expect(get(shedder.stage)).toBe("active");
    expect(events).toEqual([]);
    shedder.stop();
  });

  it("respects mode 'off' and 'aggressive' critical pressure escalation", async () => {
    resetForegroundFocus();
    const { timers, advance } = fakeClock();
    const host = fakeHost();
    const { actions, events } = spyActions();

    const shedder = createIdleShedder(actions, { initialMode: "off", doc: host.doc, win: host.win, timers });
    host.hide();
    advance(30 * 60_000);
    expect(get(shedder.stage)).toBe("active");
    expect(events).toEqual([]);

    shedder.setMode("aggressive");
    shedder.setPressure("critical");
    await Promise.resolve();
    expect(get(shedder.stage)).toBe("hibernating");
    expect(events).toEqual(["soft", "deep", "hibernate"]);
    shedder.stop();
  });
});
