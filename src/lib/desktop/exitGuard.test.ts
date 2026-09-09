import { afterEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";
import { waitForGitIdle } from "./exitGuard";

afterEach(() => vi.useRealTimers());
describe("quit waits for Git actions", () => {
  it("releases a subscription that becomes idle immediately when attached", async () => {
    let subscriptions = 0;
    const stop = vi.fn();
    await waitForGitIdle({ subscribe(run) {
      run(++subscriptions === 1 ? { a: ["fetch"] } : { a: [] });
      return stop;
    } });
    expect(stop).toHaveBeenCalledTimes(2);
  });
  it("waits for all repositories, including actions begun during the wait", async () => {
    const activity = writable({ a: ["fetch"] });
    const done = vi.fn();
    const waiting = waitForGitIdle(activity).then(done);
    activity.set({ a: ["pull", "push"] });
    await Promise.resolve(); expect(done).not.toHaveBeenCalled();
    activity.set({ a: [] }); await waiting;
    expect(done).toHaveBeenCalledOnce();
  });
  it("reports a timeout instead of treating unexamined work as idle", async () => {
    vi.useFakeTimers();
    const activity = writable({ a: ["fetch"] });
    const waiting = expect(waitForGitIdle(activity, 100)).rejects.toThrow("remain open");
    await vi.advanceTimersByTimeAsync(100);
    await waiting;
    activity.set({ a: [] });
    expect(vi.getTimerCount()).toBe(0);
  });
});
