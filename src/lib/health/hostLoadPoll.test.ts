import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { AdaptiveHost } from "../runtime/adaptiveTimer";
import { HOST_LOAD_POLL_MS, pollHostLoad } from "./hostLoadPoll";

/** A host whose clock and visibility the test drives. */
function fakeHost() {
  let now = 0;
  let hidden = false;
  let nextId = 1;
  const timers = new Map<number, { at: number; run: () => void }>();
  const listeners = new Map<string, Set<() => void>>();
  const host: AdaptiveHost = {
    setTimeout: (run, ms) => {
      const id = nextId++;
      timers.set(id, { at: now + ms, run });
      return id;
    },
    clearTimeout: (handle) => void timers.delete(handle as number),
    addEventListener: (type, listener) => {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type)?.add(listener);
    },
    removeEventListener: (type, listener) => void listeners.get(type)?.delete(listener),
    isHidden: () => hidden,
  };
  return {
    host,
    advance(ms: number) {
      const end = now + ms;
      for (;;) {
        const due = [...timers.entries()].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        now = due[1].at;
        due[1].run();
      }
      now = end;
    },
    setHidden(value: boolean) {
      hidden = value;
      for (const listener of [...(listeners.get("visibilitychange") ?? [])]) listener();
    },
    armed: () => timers.size,
    listenerCount: () => [...listeners.values()].reduce((sum, set) => sum + set.size, 0),
  };
}

describe("pollHostLoad", () => {
  it("reads now, then once per interval while visible", () => {
    const clock = fakeHost();
    let reads = 0;
    const stop = pollHostLoad(async () => void reads++, clock.host);
    expect(reads).toBe(1);
    clock.advance(HOST_LOAD_POLL_MS);
    expect(reads).toBe(2);
    clock.advance(HOST_LOAD_POLL_MS * 3);
    expect(reads).toBe(5);
    stop();
  });

  it("stops reading while the window is hidden and resumes with one immediate read", () => {
    const clock = fakeHost();
    let reads = 0;
    const stop = pollHostLoad(async () => void reads++, clock.host);
    clock.setHidden(true);
    expect(clock.armed()).toBe(0);
    clock.advance(HOST_LOAD_POLL_MS * 10);
    expect(reads).toBe(1);
    clock.setHidden(false);
    expect(reads).toBe(2);
    clock.advance(HOST_LOAD_POLL_MS);
    expect(reads).toBe(3);
    stop();
  });

  it("stop clears the timer and the listeners, and a rejected read does not break the cadence", () => {
    const clock = fakeHost();
    let reads = 0;
    const stop = pollHostLoad(async () => {
      reads++;
      throw new Error("sampler down");
    }, clock.host);
    clock.advance(HOST_LOAD_POLL_MS);
    expect(reads).toBe(2);
    stop();
    expect(clock.armed()).toBe(0);
    expect(clock.listenerCount()).toBe(0);
    clock.advance(HOST_LOAD_POLL_MS * 5);
    expect(reads).toBe(2);
  });
});

describe("HostLoadInputSetting", () => {
  const source = readFileSync(new URL("../components/HostLoadInputSetting.svelte", import.meta.url), "utf8");

  it("polls through pollHostLoad, never a bare interval", () => {
    expect(source).toContain("return pollHostLoad();");
    expect(source).not.toMatch(/\bsetInterval\s*\(/);
  });

  it("draws its mode chooser with gp-select", () => {
    expect(source).toMatch(/<select[^>]*class="gp-select /s);
  });
});
