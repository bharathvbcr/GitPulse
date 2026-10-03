import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createPacedQueue } from "./pacedQueue";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

const limits = { debounceMs: 200, maxWaitMs: 1_000, restMs: 1_000, capacity: 64 };

it("retains hidden-window changes without starting work or scheduling wakeups", async () => {
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queue = createPacedQueue({
    ...limits, run, onError: vi.fn(), onOverflow: vi.fn(),
    scope: { activeKey: "/a", retainedKeys: ["/a", "/b"], visible: false },
  });
  for (let i = 0; i < 10_000; i++) queue.enqueue(i % 2 ? "/a" : "/b");
  await vi.advanceTimersByTimeAsync(60_000);
  expect(run).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
  expect(queue.isPending("/a")).toBe(true);
  expect(queue.isPending("/b")).toBe(true);
  queue.reset();
});

it("retained mode runs every open repository while the window is visible", async () => {
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queue = createPacedQueue({
    ...limits, runWhen: "retained", run, onError: vi.fn(), onOverflow: vi.fn(),
    scope: { activeKey: "/a", retainedKeys: ["/a", "/b"], visible: true },
  });
  queue.enqueue("/b");
  queue.enqueue("/a");
  // This used to expect ["/b", "/a"]: insertion order, which put the focused
  // repository behind every tab opened before it. Both still run; the
  // focused one now goes first (see the launch test below).
  await vi.advanceTimersByTimeAsync(200);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/a"]);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/a", "/b"]);
  queue.reset();
});

it("runs the focused repository first when every restored tab is due at once", async () => {
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const tabs = ["/one", "/two", "/three", "/four"];
  const queue = createPacedQueue({
    ...limits, runWhen: "retained", run, onError: vi.fn(), onOverflow: vi.fn(),
    scope: { activeKey: "/four", retainedKeys: tabs, visible: true },
  });
  // Launch: tabs are offered in tab order, and the focused tab is the last.
  for (const key of tabs) queue.enqueue(key);
  await vi.advanceTimersByTimeAsync(200);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/four"]);
  // Focus moves while the rest are still queued: the new focus jumps the line.
  queue.setScope({ activeKey: "/three", retainedKeys: tabs, visible: true });
  await vi.advanceTimersByTimeAsync(1_000);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/four", "/three"]);
  await vi.advanceTimersByTimeAsync(2_500);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/four", "/three", "/one", "/two"]);
  queue.reset();
});

it("a delayed retry is not pulled forward by a later enqueue, nor clamped by maxWait", async () => {
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queue = createPacedQueue({
    ...limits, runWhen: "retained", run, onError: vi.fn(), onOverflow: vi.fn(),
    scope: { activeKey: "/a", retainedKeys: ["/a"], visible: true },
  });
  expect(queue.enqueue("/a", { delayMs: 30_000 })).toBe(true);
  // A watcher tick lands inside the backoff: it joins the pending retry
  // instead of resetting it to the debounce.
  await vi.advanceTimersByTimeAsync(5_000);
  queue.enqueue("/a");
  await vi.advanceTimersByTimeAsync(24_000);
  expect(run).not.toHaveBeenCalled();
  expect(queue.isPending("/a")).toBe(true);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(run).toHaveBeenCalledExactlyOnceWith("/a", expect.any(Function));
  // The hold belongs to that one retry; the next plain enqueue is debounced.
  queue.enqueue("/a");
  await vi.advanceTimersByTimeAsync(1_200);
  expect(run).toHaveBeenCalledTimes(2);
  queue.reset();
});

it("does not run an inactive repository or a late event for a closed repository", async () => {
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queue = createPacedQueue({
    ...limits, run, onError: vi.fn(), onOverflow: vi.fn(),
    scope: { activeKey: "/a", retainedKeys: ["/a", "/b"], visible: true },
  });
  queue.enqueue("/b");
  expect(queue.enqueue("/closed")).toBe(false);
  queue.enqueue("/a");
  await vi.advanceTimersByTimeAsync(60_000);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/a"]);
  expect(queue.isPending("/b")).toBe(true);
  expect(vi.getTimerCount()).toBe(0);
  queue.reset();
});

it("resumes only the active dirty key after an activation grace period", async () => {
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queue = createPacedQueue({ ...limits, run, onError: vi.fn(), onOverflow: vi.fn() });
  const retainedKeys = ["/a", "/b"];
  queue.setScope({ activeKey: "/a", retainedKeys, visible: false });
  for (let i = 0; i < 10_000; i++) queue.enqueue(i % 2 ? "/a" : "/b");
  await vi.advanceTimersByTimeAsync(60_000);
  queue.setScope({ activeKey: "/a", retainedKeys, visible: true });
  await vi.advanceTimersByTimeAsync(199);
  expect(run).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/a"]);
  queue.setScope({ activeKey: "/b", retainedKeys, visible: true });
  await vi.advanceTimersByTimeAsync(999);
  expect(run).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/a", "/b"]);
  queue.reset();
});

it("closing and reopening an active operation invalidates its result without overlap", async () => {
  let release!: () => void;
  let stillCurrent!: () => boolean;
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>()
    .mockImplementationOnce((_key, isCurrent) => {
      stillCurrent = isCurrent;
      return new Promise((done) => { release = done; });
    }).mockResolvedValue(undefined);
  const queue = createPacedQueue({ ...limits, run, onError: vi.fn(), onOverflow: vi.fn() });
  queue.setScope({ activeKey: "/a", retainedKeys: ["/a"], visible: true });
  queue.enqueue("/a");
  await vi.advanceTimersByTimeAsync(200);
  expect(stillCurrent()).toBe(true);
  queue.enqueue("/a");
  queue.setScope({ activeKey: null, retainedKeys: [], visible: false });
  expect(queue.isPending("/a")).toBe(false);
  queue.setScope({ activeKey: "/a", retainedKeys: ["/a"], visible: true });
  queue.enqueue("/a");
  expect(stillCurrent()).toBe(false);
  await vi.advanceTimersByTimeAsync(10_000);
  expect(run).toHaveBeenCalledTimes(1);
  release();
  await vi.advanceTimersByTimeAsync(1_000);
  expect(run).toHaveBeenCalledTimes(2);
  queue.reset();
});

it("hiding during a scan retains its follow-up without a timer or stale publication", async () => {
  let release!: () => void;
  let current!: () => boolean;
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>()
    .mockImplementationOnce((_key, isCurrent) => {
      current = isCurrent;
      return new Promise((done) => { release = done; });
    }).mockResolvedValue(undefined);
  const queue = createPacedQueue({ ...limits, run, onError: vi.fn(), onOverflow: vi.fn() });
  const scope = { activeKey: "/a", retainedKeys: ["/a"], visible: true };
  queue.setScope(scope);
  queue.enqueue("/a");
  await vi.advanceTimersByTimeAsync(200);
  queue.setScope({ ...scope, visible: false });
  queue.enqueue("/a");
  release();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(current()).toBe(true); // Visibility alone does not invalidate a completed scan.
  expect(run).toHaveBeenCalledTimes(1);
  expect(vi.getTimerCount()).toBe(0);
  queue.setScope(scope);
  await vi.advanceTimersByTimeAsync(200);
  expect(run).toHaveBeenCalledTimes(2);
  queue.reset();
});

it("copies scope inputs and recovers queue capacity when repositories close", async () => {
  const overflow = vi.fn();
  const run = vi.fn<(key: string, isCurrent: () => boolean) => Promise<void>>(async () => {});
  const queue = createPacedQueue({ ...limits, capacity: 2, run, onError: vi.fn(), onOverflow: overflow });
  const retainedKeys = ["/a", "/b", "/c"];
  queue.setScope({ activeKey: "/a", retainedKeys, visible: false });
  retainedKeys.length = 0;
  expect(queue.enqueue("/a")).toBe(true);
  expect(queue.enqueue("/b")).toBe(true);
  expect(queue.enqueue("/c")).toBe(false);
  expect(overflow).toHaveBeenCalledTimes(1);
  queue.setScope({ activeKey: "/c", retainedKeys: ["/c"], visible: true });
  expect(queue.enqueue("/c")).toBe(true);
  await vi.advanceTimersByTimeAsync(200);
  expect(run.mock.calls.map(([key]) => key)).toEqual(["/c"]);
  queue.reset();
});
