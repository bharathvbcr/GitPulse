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
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
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
});
