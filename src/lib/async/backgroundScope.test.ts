import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { writable } from "svelte/store";
import { installBackgroundScope } from "./backgroundScope";
import { createPacedQueue, type BackgroundScope } from "./pacedQueue";

class Visibility extends EventTarget {
  visibilityState: DocumentVisibilityState = "visible";
  set(value: DocumentVisibilityState) {
    this.visibilityState = value;
    this.dispatchEvent(new Event("visibilitychange"));
  }
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

it("updates both queue consumers on workspace/visibility changes and unwires on teardown", () => {
  const workspace = writable({ currentPath: "/a", openTabs: [{ path: "/a" }, { path: "/b" }] });
  const target = new Visibility();
  const apply = vi.fn<(scope: BackgroundScope) => void>();
  const dispose = installBackgroundScope({ subscribe: workspace.subscribe, target, apply });
  expect(apply).toHaveBeenCalledExactlyOnceWith({ activeKey: "/a", retainedKeys: ["/a", "/b"], visible: true });
  for (let i = 0; i < 10_000; i++) workspace.update((state) => ({ ...state }));
  expect(apply).toHaveBeenCalledTimes(1);
  target.set("hidden");
  expect(apply).toHaveBeenLastCalledWith({ activeKey: "/a", retainedKeys: ["/a", "/b"], visible: false });
  workspace.update((state) => ({ ...state, currentPath: "/b" }));
  expect(apply).toHaveBeenCalledTimes(3);
  target.set("visible");
  expect(apply).toHaveBeenCalledTimes(4);
  dispose();
  dispose();
  expect(apply).toHaveBeenLastCalledWith({ activeKey: null, retainedKeys: [], visible: false });
  workspace.update((state) => ({ ...state, currentPath: "/a" }));
  target.set("hidden");
  expect(apply).toHaveBeenCalledTimes(5);
  expect(vi.getTimerCount()).toBe(0);
});

it("keeps two independent queues dormant through a 50,000-event background storm", async () => {
  const target = new Visibility();
  target.set("hidden");
  const workspace = writable({ currentPath: "/a", openTabs: [{ path: "/a" }, { path: "/b" }] });
  const calls = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queues = Array.from({ length: 2 }, () => createPacedQueue({
    debounceMs: 200, maxWaitMs: 1_000, restMs: 1_000, capacity: 64,
    run: calls, onError: vi.fn(), onOverflow: vi.fn(),
  }));
  const dispose = installBackgroundScope({ subscribe: workspace.subscribe, target,
    apply: (scope) => { for (const queue of queues) queue.setScope(scope); },
  });
  for (let i = 0; i < 50_000; i++) {
    for (const queue of queues) queue.enqueue(i % 2 ? "/a" : "/b");
  }
  await vi.advanceTimersByTimeAsync(120_000);
  expect(calls).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
  target.set("visible");
  await vi.advanceTimersByTimeAsync(200);
  expect(calls.mock.calls.map(([key]) => key)).toEqual(["/a", "/a"]);
  dispose();
  await vi.advanceTimersByTimeAsync(120_000);
  expect(calls).toHaveBeenCalledTimes(2);
  expect(queues.every((queue) => !queue.isPending("/b"))).toBe(true);
});
