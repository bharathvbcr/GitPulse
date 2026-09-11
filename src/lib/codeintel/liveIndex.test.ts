/**
 * Live-index controller: debounce, one-in-flight, publish strip state.
 *
 * Gate decisions (stale→refresh / fresh→skip / in-flight→skip) are owned by
 * the Rust `decide_live_refresh` tests; this suite covers the TS scheduler.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createLiveIndex } from "./liveIndex";
import type { LiveRefreshOutcome } from "./types";

const warnings = vi.hoisted(() => vi.fn());
vi.mock("../diagnostics/diagnostics", () => ({ diagnostics: { warn: warnings } }));

function outcome(
  decision: LiveRefreshOutcome["decision"],
  extras: Partial<LiveRefreshOutcome> = {},
): LiveRefreshOutcome {
  return {
    decision,
    facts: {
      available: true,
      is_fresh: decision === "skip_fresh",
      schema_ok: decision !== "skip_schema_outdated",
      already_building: decision === "skip_building",
    },
    build:
      decision === "refresh"
        ? {
            ok: true,
            binary: "/bin/devmap",
            lookup: "path_search",
            exit_code: 0,
            stdout: "{}",
            stderr: "",
            timed_out: false,
          }
        : null,
    reason: null,
    ...extras,
  };
}

describe("liveIndex controller", () => {
  it("a busy repository yields to other work and resumes after becoming visible", async () => {
    const maybeRefresh = vi.fn(async (repo: string) => outcome(repo === "/busy" ? "skip_building" : "refresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/busy");
    index.onRepoChanged("/other");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(index.get("/other").phase).toBe("ready");
    index.setScope({ activeKey: "/busy", retainedKeys: ["/busy"], visible: false });
    const calls = maybeRefresh.mock.calls.length;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(calls);
    expect(vi.getTimerCount()).toBe(0);
    maybeRefresh.mockResolvedValue(outcome("refresh"));
    index.setScope({ activeKey: "/busy", retainedKeys: ["/busy"], visible: true });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(index.get("/busy").phase).toBe("ready");
    index.reset();
  });

  it("closing or resetting during a busy result never revives its retry", async () => {
    for (const close of [false, true]) {
      let release!: (value: LiveRefreshOutcome) => void;
      const maybeRefresh = vi.fn(() => new Promise<LiveRefreshOutcome>(resolve => { release = resolve; }));
      const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
      index.onRepoChanged("/repo");
      await vi.advanceTimersByTimeAsync(0);
      if (close) index.setScope({ activeKey: null, retainedKeys: [], visible: true });
      else index.reset();
      release(outcome("skip_building"));
      await vi.advanceTimersByTimeAsync(120_000);
      expect(maybeRefresh).toHaveBeenCalledTimes(1);
      expect(index.get("/repo").phase).toBe("idle");
      expect(vi.getTimerCount()).toBe(0);
      index.reset();
    }
  });

  it("continuous edit storms cannot postpone all builds until the writer stops", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ maybeRefresh });
    for (let tick = 0; tick < 2_000; tick++) {
      index.onRepoChanged("/repo");
      await vi.advanceTimersByTimeAsync(5);
    }
    expect(maybeRefresh.mock.calls.length).toBeGreaterThanOrEqual(5);
    expect(maybeRefresh.mock.calls.length).toBeLessThanOrEqual(10);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(index.get("/repo").phase).toBe("ready");
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("retries a dirty event skipped while a manual build owns the writer", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"))
      .mockResolvedValueOnce(outcome("skip_building"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("scheduled");
    await vi.advanceTimersByTimeAsync(2_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    expect(index.get("/repo").phase).toBe("ready");
    expect(index.get("/repo").revision).toBe(1);
    index.reset();
  });

  it("bounds busy retries and reports failure without inventing a publication", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_building"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/busy");
    await vi.advanceTimersByTimeAsync(120_000);
    expect(maybeRefresh.mock.calls.length).toBeGreaterThan(1);
    expect(maybeRefresh.mock.calls.length).toBeLessThanOrEqual(31);
    expect(index.get("/busy").phase).toBe("failed");
    expect(index.get("/busy").reason).toContain("busy");
    expect(index.get("/busy").revision).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("retains inactive updates and removes closed snapshots including late completions", async () => {
    let release!: (result: LiveRefreshOutcome) => void;
    const maybeRefresh = vi.fn(() => Promise.resolve(outcome("refresh")))
      .mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh,
      scope: { activeKey: "/a", retainedKeys: ["/a", "/b"], visible: false },
    });
    index.onRepoChanged("/a");
    index.onRepoChanged("/b");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(maybeRefresh).not.toHaveBeenCalled();
    expect(index.get("/b").phase).toBe("scheduled");
    index.setScope({ activeKey: "/a", retainedKeys: ["/a", "/b"], visible: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/a", true);
    index.setScope({ activeKey: "/b", retainedKeys: ["/b"], visible: true });
    release(outcome("refresh"));
    await vi.advanceTimersByTimeAsync(1_000);
    expect(index.get("/a").phase).toBe("idle");
    expect(get(index.snapshots)).not.toHaveProperty("/a");
    expect(index.get("/b").phase).toBe("ready");
    index.onRepoChanged("/closed");
    expect(get(index.snapshots)).not.toHaveProperty("/closed");
    index.reset();
  });

  it("publishes successful content revisions even with frozen time and a queued follow-up", async () => {
    let release!: (value: LiveRefreshOutcome) => void;
    const maybeRefresh = vi.fn(() => Promise.resolve(outcome("skip_fresh")))
      .mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").revision).toBe(0);
    index.onRepoChanged("/repo");
    release(outcome("refresh"));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("scheduled");
    expect(index.get("/repo").revision).toBe(1);
    await vi.advanceTimersByTimeAsync(2_000);
    expect(index.get("/repo").phase).toBe("skipped");
    expect(index.get("/repo").revision).toBe(1);
    index.reset();
  });

  it("never reports a refresh without a build outcome as ready", async () => {
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh: async () => outcome("refresh", { build: null }) });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("failed");
    expect(index.get("/repo").revision).toBe(0);
    index.reset();
  });

  it("does not bump revision when a successful build reports unchanged", async () => {
    const index = createLiveIndex({
      debounceMs: 0,
      maybeRefresh: async () =>
        outcome("refresh", {
          build: {
            ok: true,
            binary: "/bin/devmap",
            lookup: "path_search",
            exit_code: 0,
            stdout: "{}",
            stderr: "",
            timed_out: false,
            report: { unchanged: true },
          },
        }),
    });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("ready");
    expect(index.get("/repo").revision).toBe(0);
    index.reset();
  });

  it("still publishes a revision when unchanged is absent from a successful build", async () => {
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh: async () => outcome("refresh") });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("ready");
    expect(index.get("/repo").revision).toBe(1);
    index.reset();
  });

  it("surfaces skip_cooldown as scheduled without busy-retrying", async () => {
    const maybeRefresh = vi.fn(async () =>
      outcome("skip_cooldown", {
        reason: "live index cooldown active; retry in 1000ms",
        cooldown_remaining_ms: 1000,
      }),
    );
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("scheduled");
    expect(index.get("/repo").decision).toBe("skip_cooldown");
    expect(index.get("/repo").reason).toContain("cooldown");
    await vi.advanceTimersByTimeAsync(5_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    index.reset();
  });
  beforeEach(() => {
    vi.useFakeTimers();
    warnings.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("retains a follow-up after changes during an in-flight build", async () => {
    let release!: (value: LiveRefreshOutcome) => void;
    const maybeRefresh = vi.fn(() => Promise.resolve(outcome("refresh")))
      .mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/busy");
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 100; i++) index.onRepoChanged("/busy");
    await vi.advanceTimersByTimeAsync(100);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    release(outcome("refresh"));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    index.reset();
  });

  it("serializes repositories and keeps the running slot across reset", async () => {
    let release!: (value: LiveRefreshOutcome) => void;
    const maybeRefresh = vi.fn(() => Promise.resolve(outcome("refresh")))
      .mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    for (let i = 0; i < 32; i++) index.onRepoChanged(`/repo/${i}`);
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    index.reset();
    index.onRepoChanged("/new");
    await vi.advanceTimersByTimeAsync(100);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    release(outcome("refresh"));
    await vi.advanceTimersByTimeAsync(2_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    expect(index.get("/repo/0").phase).toBe("idle");
    expect(index.get("/new").phase).toBe("ready");
    index.reset();
  });

  it("continuous events cannot starve the index or build continuously", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ maybeRefresh });
    for (let i = 0; i < 60; i++) {
      index.onRepoChanged("/busy");
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(maybeRefresh.mock.calls.length).toBeGreaterThanOrEqual(4);
    expect(maybeRefresh.mock.calls.length).toBeLessThanOrEqual(6);
    index.reset();
  });

  it("bounds flood admission, timer count, and retained snapshots", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ maybeRefresh });
    for (let i = 0; i < 1_000; i++) index.onRepoChanged(`/repo/${i}`);
    expect(vi.getTimerCount()).toBe(1);
    expect(warnings).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(66_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(64);
    for (let i = 1_000; i < 1_100; i++) {
      index.onRepoChanged(`/repo/${i}`);
      await vi.advanceTimersByTimeAsync(1_200);
    }
    expect(Object.keys(get(index.snapshots)).length).toBeLessThanOrEqual(65);
    expect(index.get("/repo/1099").phase).toBe("ready");
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("failure does not strand queued repositories", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"))
      .mockRejectedValueOnce(new Error("dependency unavailable"));
    const index = createLiveIndex({ maybeRefresh });
    index.onRepoChanged("/failed");
    index.onRepoChanged("/next");
    await vi.advanceTimersByTimeAsync(2_000);
    expect(index.get("/failed").phase).toBe("failed");
    expect(index.get("/next").phase).toBe("ready");
    index.reset();
  });

  it("rejects invalid timer configuration", () => {
    for (const debounceMs of [NaN, Infinity, -1]) {
      expect(() => createLiveIndex({ debounceMs })).toThrow(RangeError);
    }
    expect(vi.getTimerCount()).toBe(0);
  });

  it("stale → schedules a refresh after debounce", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ debounceMs: 50, maybeRefresh });
    index.onRepoChanged("/repo/a");
    expect(get(index.snapshots)["/repo/a"]?.phase).toBe("scheduled");
    await vi.advanceTimersByTimeAsync(50);
    await Promise.resolve();
    expect(maybeRefresh).toHaveBeenCalledWith("/repo/a", true);
    expect(get(index.snapshots)["/repo/a"]?.phase).toBe("ready");
    expect(get(index.snapshots)["/repo/a"]?.decision).toBe("refresh");
    index.reset();
  });

  it("fresh → skip is published without treating it as failure", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 10, maybeRefresh });
    index.onRepoChanged("/repo/b");
    await vi.advanceTimersByTimeAsync(10);
    await Promise.resolve();
    expect(get(index.snapshots)["/repo/b"]?.phase).toBe("skipped");
    expect(get(index.snapshots)["/repo/b"]?.decision).toBe("skip_fresh");
    expect(get(index.snapshots)["/repo/b"]?.refreshing).toBe(false);
    index.reset();
  });

  it("in-flight → skip when a second run is still busy", async () => {
    let release!: (value: LiveRefreshOutcome) => void;
    const maybeRefresh = vi.fn(
      () =>
        new Promise<LiveRefreshOutcome>((resolve) => {
          release = resolve;
        }),
    );
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo/c");
    await vi.advanceTimersByTimeAsync(0);
    await Promise.resolve();
    expect(get(index.snapshots)["/repo/c"]?.phase).toBe("running");
    expect(get(index.snapshots)["/repo/c"]?.refreshing).toBe(true);

    // A second tick while the first is running must not start another child.
    index.onRepoChanged("/repo/c");
    await vi.advanceTimersByTimeAsync(0);
    await Promise.resolve();
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    expect(get(index.snapshots)["/repo/c"]?.phase).toBe("running");

    release(outcome("skip_building"));
    await Promise.resolve();
    expect(get(index.snapshots)["/repo/c"]?.decision).toBe("skip_building");
    index.reset();
  });

    it("coalesces a watcher storm into one maybeRefresh", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ debounceMs: 40, maybeRefresh });
    index.onRepoChanged("/repo/d");
    index.onRepoChanged("/repo/d");
    index.onRepoChanged("/repo/d");
    await vi.advanceTimersByTimeAsync(40);
    await Promise.resolve();
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    index.reset();
  });

  it("heals every retained repository, not only the focused one", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.setScope({
      activeKey: "/devcouncil",
      retainedKeys: ["/devcouncil", "/manvi"],
      visible: true,
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/devcouncil", false);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(maybeRefresh.mock.calls.map((call) => call[0])).toEqual(["/devcouncil", "/manvi"]);
    expect(maybeRefresh.mock.calls.every((call) => call[1] === false)).toBe(true);
    index.setScope({
      activeKey: "/manvi",
      retainedKeys: ["/devcouncil", "/manvi"],
      visible: true,
    });
    await vi.advanceTimersByTimeAsync(120_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    index.setScope({
      activeKey: "/gitpulse",
      retainedKeys: ["/gitpulse"],
      visible: true,
    });
    await vi.advanceTimersByTimeAsync(1_000);
    index.setScope({
      activeKey: "/manvi",
      retainedKeys: ["/manvi"],
      visible: true,
    });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(maybeRefresh.mock.calls.filter((call) => call[0] === "/manvi")).toHaveLength(2);
    index.reset();
  });

  it("asks a status-only refresh when a repository becomes visible without a watcher event", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/repo", false);
    expect(index.get("/repo").phase).toBe("skipped");
    index.reset();
  });

  it("repeated setScope of the same visible repository does not re-enqueue", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    for (let i = 0; i < 32; i++) {
      index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
    }
    await vi.advanceTimersByTimeAsync(120_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/repo", false);
    index.reset();
  });

  it("a watcher tick still reports repoChanged even when activation is also pending", async () => {
    const maybeRefresh = vi.fn(async () => outcome("refresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/repo", true);
    index.reset();
  });

  it("activation never reports a dirty tree, including across a storm of focus changes", async () => {
    const maybeRefresh = vi.fn(async (_repo: string, _repoChanged: boolean) => outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    for (let i = 0; i < 64; i++) {
      index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
      index.setScope({ activeKey: "/other", retainedKeys: ["/other"], visible: true });
    }
    index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
    await vi.advanceTimersByTimeAsync(120_000);
    expect(maybeRefresh.mock.calls.length).toBeGreaterThan(0);
    expect(maybeRefresh.mock.calls.every((call) => call[1] === false)).toBe(true);
    index.reset();
  });
});
