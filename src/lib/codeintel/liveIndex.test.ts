/**
 * Live-index controller: debounce, one-in-flight, publish strip state.
 *
 * Gate decisions (stale→refresh / fresh→skip / in-flight→skip) are owned by
 * the Rust `decide_live_refresh` tests; this suite covers the TS scheduler.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import {
  busyRetryDelayMs,
  createLiveIndex,
  LIVE_INDEX_BUILD_WAIT_MS,
  LIVE_INDEX_BUSY_RETRIES,
  LIVE_INDEX_COOLDOWN_REASKS,
} from "./liveIndex";
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

  it("backs off a busy writer instead of asking once a second", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_building"));
    // The base backoff, unspread: the spread has its own test below.
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh, random: () => 0 });
    index.onRepoChanged("/busy");
    // Asks at 0, 1, 3, 7, 15 and 31 s: a fixed one-second rest asked ~30 times.
    await vi.advanceTimersByTimeAsync(30_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(5);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(6);
    // Still retrying where the fixed rest had already given up.
    expect(index.get("/busy").phase).toBe("scheduled");
    index.reset();
  });

  it("spreads a busy writer's retry upward, through the shared jitter", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_building"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh, random: () => 1 });
    index.onRepoChanged("/busy");
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    // 1 s lengthened by the whole jitter: not before 1.5 s.
    await vi.advanceTimersByTimeAsync(1_400);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(200);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    index.reset();
  });

  it("schedules busy retries 1 s apart, doubling, capped", () => {
    expect([1, 2, 3, 4, 5, 6, 30, 1e9].map(busyRetryDelayMs))
      .toEqual([1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]);
    expect(busyRetryDelayMs(0)).toBe(1_000);
    expect(busyRetryDelayMs(Number.NaN)).toBe(1_000);
  });

  it("bounds busy retries and reports failure without inventing a publication", async () => {
    const maybeRefresh = vi.fn(async () => outcome("skip_building"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/busy");
    await vi.advanceTimersByTimeAsync(20 * 60_000);
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

  it("records build stages only while a build is actually running", async () => {
    // The kernel prints `[n/5]`; a cold index of a large repository is minutes
    // of an empty pane without it. But a stray event must not resurrect a row:
    // only a build this controller believes is running may move the bar.
    let resolve!: (value: LiveRefreshOutcome) => void;
    const index = createLiveIndex({
      debounceMs: 0,
      maybeRefresh: () =>
        new Promise<LiveRefreshOutcome>((r) => {
          resolve = r;
        }),
    });

    // Before anything runs, progress is ignored.
    index.onBuildProgress("/repo", 2, 5);
    expect(index.get("/repo").stage).toBeNull();

    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("running");
    // A running build starts with no stage rather than a fabricated first one.
    expect(index.get("/repo").stage).toBeNull();

    index.onBuildProgress("/repo", 3, 5);
    expect(index.get("/repo").stage).toEqual({ current: 3, total: 5 });

    // Nonsense from a future producer is dropped, not rendered.
    index.onBuildProgress("/repo", Number.NaN, 5);
    index.onBuildProgress("/repo", 1, 0);
    expect(index.get("/repo").stage).toEqual({ current: 3, total: 5 });

    // A different repository's event never lands on this row.
    index.onBuildProgress("/other", 1, 5);
    expect(index.get("/repo").stage).toEqual({ current: 3, total: 5 });

    resolve(outcome("refresh"));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("ready");
    expect(index.get("/repo").stage).toBeNull();
    index.reset();
  });

  it("publishes a revision when an unchanged build restored the map artifacts", async () => {
    // The cold-start and self-heal path: `devmap build --manifest` rewrites
    // `repo_map.json` from the existing generation, so the store is genuinely
    // `unchanged` while the document the Map pane reads is new. Suppressing
    // the revision here left the pane showing nothing until a manual reload.
    const index = createLiveIndex({
      debounceMs: 0,
      maybeRefresh: async () =>
        outcome("refresh", {
          artifacts_restored: true,
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
    expect(index.get("/repo").revision).toBe(1);
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

  it("re-asks a watcher tick once when the backend's wait ends, never an activation, and bounds the chain", async () => {
    // A watcher tick answered `skip_cooldown` used to be dropped: the edit
    // that landed inside the wait was indexed only if another edit came
    // after it. It is owed one ask when the wait ends — not a busy spin.
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) =>
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
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    // Not before the wait ends.
    await vi.advanceTimersByTimeAsync(500);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(600);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    expect(maybeRefresh.mock.calls.every(([, changed]) => changed === true)).toBe(true);
    // A backend that keeps answering with a fresh wait is asked a bounded
    // number of times, then the next edit is the next request.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(1 + LIVE_INDEX_COOLDOWN_REASKS);
    expect(vi.getTimerCount()).toBe(0);
    index.reset();

    // Activation inside a wait is not owed a build.
    maybeRefresh.mockClear();
    index.setScope({ activeKey: "/other", retainedKeys: ["/other"], visible: true });
    await vi.advanceTimersByTimeAsync(60_000);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/other", false);
    index.reset();
  });

  it("folds a 10-minute watcher storm into one ask per backend rest window and loses no edit", async () => {
    // The backend governor in miniature: a build rests 30 s, and a watcher
    // tick inside the rest is answered `skip_cooldown` without a build.
    let restUntil = 0;
    const builds: number[] = [];
    const maybeRefresh = vi.fn(async (_repo: string, repoChanged: boolean) => {
      const now = Date.now();
      if (repoChanged && now < restUntil) {
        return outcome("skip_cooldown", { reason: "resting", cooldown_remaining_ms: restUntil - now });
      }
      builds.push(now);
      restUntil = now + 30_000;
      return outcome("refresh", { cooldown_remaining_ms: 30_000 });
    });
    // Status poll and watcher both route through `onRepoChanged`, once a
    // second each, for ten minutes.
    const index = createLiveIndex({ maybeRefresh });
    for (let second = 0; second < 600; second++) {
      index.onRepoChanged("/repo");
      index.onRepoChanged("/repo");
      await vi.advanceTimersByTimeAsync(1_000);
    }
    const lastEdit = Date.now() - 1_000;
    await vi.advanceTimersByTimeAsync(60_000);
    // One ask per window, not one per edit (1,200 edits; ~20 windows).
    expect(maybeRefresh.mock.calls.length).toBeLessThanOrEqual(45);
    expect(builds.length).toBeGreaterThanOrEqual(19);
    expect(builds.length).toBeLessThanOrEqual(22);
    // The storm's last edit is indexed: dirt inside a window is owed one
    // build at its end, rather than waiting for an edit that never comes.
    expect(builds[builds.length - 1]).toBeGreaterThanOrEqual(lastEdit);
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("collects a build the backend started in the background, keeping the stage bar, without spending busy retries", async () => {
    // The command no longer holds the IPC call for the build: it answers
    // `started`, and asks while the child runs answer `skip_building`. More of
    // those than the busy budget allows must not fail a build that is simply
    // still running.
    const answers: LiveRefreshOutcome[] = [
      outcome("started"),
      ...Array.from({ length: LIVE_INDEX_BUSY_RETRIES + 10 }, () => outcome("skip_building")),
      outcome("refresh", { cooldown_remaining_ms: 30_000 }),
    ];
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => answers.shift() ?? outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("running");
    expect(index.get("/repo").refreshing).toBe(true);
    index.onBuildProgress("/repo", 2, 5);
    await vi.advanceTimersByTimeAsync(60_000);
    // Still collecting, and the kernel's stage survives every ask.
    expect(index.get("/repo").phase).toBe("running");
    expect(index.get("/repo").stage).toEqual({ current: 2, total: 5 });
    await vi.advanceTimersByTimeAsync(10 * 60_000);
    expect(index.get("/repo").phase).toBe("ready");
    expect(index.get("/repo").revision).toBe(1);
    expect(maybeRefresh).toHaveBeenCalledTimes(LIVE_INDEX_BUSY_RETRIES + 12);
    // Asks while the build ran were collection, not new requests: the build
    // took the edit, so none of them reported a dirty tree after the first.
    expect(maybeRefresh.mock.calls.slice(1).every(([, changed]) => changed === false)).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("another repository waits, without asking, while a background build is collected", async () => {
    // The queue slot used to serialize builds by holding the IPC call. With
    // the build off the IPC path, a second repository asking during it was
    // refused by the backend's one-build slot after paying for a status
    // probe, then spent its busy budget against a writer that was not its own.
    const answers: Record<string, LiveRefreshOutcome[]> = {
      "/a": [
        outcome("started"),
        ...Array.from({ length: 12 }, () => outcome("skip_building")),
        outcome("refresh", { cooldown_remaining_ms: 30_000 }),
      ],
      "/b": [outcome("refresh", { cooldown_remaining_ms: 30_000 })],
    };
    const maybeRefresh = vi.fn(async (repo: string, _changed: boolean) => answers[repo]?.shift() ?? outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/a");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/a").phase).toBe("running");
    index.onRepoChanged("/b");
    const callsFor = (repo: string) => maybeRefresh.mock.calls.filter(([r]) => r === repo);
    await vi.advanceTimersByTimeAsync(90_000);
    expect(callsFor("/b")).toHaveLength(0);
    expect(index.get("/b").phase).toBe("scheduled");
    await vi.advanceTimersByTimeAsync(5 * 60_000);
    expect(index.get("/a").phase).toBe("ready");
    // Once, after the build it waited behind, and still owed its edit.
    expect(callsFor("/b")).toEqual([["/b", true]]);
    expect(index.get("/b").phase).toBe("ready");
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("gives up collecting a background build that outlives its deadline", async () => {
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => outcome("skip_building"))
      .mockResolvedValueOnce(outcome("started"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(LIVE_INDEX_BUILD_WAIT_MS + 60_000);
    expect(index.get("/repo").phase).toBe("failed");
    expect(index.get("/repo").revision).toBe(0);
    // Bounded: at most one ask every ten seconds for the deadline.
    expect(maybeRefresh.mock.calls.length).toBeLessThanOrEqual(LIVE_INDEX_BUILD_WAIT_MS / 10_000 + 8);
    expect(vi.getTimerCount()).toBe(0);
    index.reset();
  });

  it("a timed-out build pauses the strip as too slow rather than queued, and asks again only when the pause ends", async () => {
    const tooSlow =
      "Index too slow: the last automatic build ran past its 15-minute limit, so automatic indexing is paused for 10 min. Press Build to index now.";
    const answers: LiveRefreshOutcome[] = [
      outcome("refresh", {
        build: {
          ok: false, binary: "/bin/devmap", lookup: "path_search", exit_code: null,
          stdout: "", stderr: "devmap timed out after 900s", timed_out: true,
        },
        reason: tooSlow,
        index_too_slow: true,
        cooldown_remaining_ms: 600_000,
      }),
      outcome("refresh", { cooldown_remaining_ms: 30_000 }),
    ];
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => answers.shift() ?? outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("paused");
    expect(index.get("/repo").reason).toContain("Index too slow");
    expect(index.get("/repo").revision).toBe(0);
    // Agents keep editing through the pause; nothing is asked until it ends.
    for (let i = 0; i < 9 * 60; i++) {
      index.onRepoChanged("/repo");
      await vi.advanceTimersByTimeAsync(1_000);
      // Still too slow, not "queued", for as long as the pause lasts.
      expect(index.get("/repo").phase).toBe("paused");
    }
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2 * 60_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    expect(index.get("/repo").phase).toBe("ready");
    index.reset();
  });

  it("a pause answered to an activation still reads as too slow", async () => {
    const maybeRefresh = vi.fn(async () =>
      outcome("skip_cooldown", {
        reason: "Index too slow: paused for 7 min.",
        index_too_slow: true,
        cooldown_remaining_ms: 420_000,
      }),
    );
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/repo").phase).toBe("paused");
    expect(index.get("/repo").reason).toContain("too slow");
    await vi.advanceTimersByTimeAsync(30 * 60_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    index.reset();
  });

  it("hiding the window does not make an unfocused tab's watcher edits live", async () => {
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    const tabs = ["/focused", "/background"];
    index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(5_000);
    maybeRefresh.mockClear();
    index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: false });
    index.onRepoChanged("/background");
    index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(maybeRefresh).not.toHaveBeenCalled();
    index.reset();
  });

  it("holds a watcher tick for a tab that is not focused until it is activated", async () => {
    // Every open worktree tab an agent edits used to rebuild in turn. Only the
    // focused one is live; the others are owed one build when focused.
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => outcome("skip_fresh"));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    const tabs = ["/focused", "/background"];
    index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(5_000);
    maybeRefresh.mockClear();
    for (let i = 0; i < 30; i++) {
      index.onRepoChanged("/background");
      await vi.advanceTimersByTimeAsync(1_000);
    }
    expect(maybeRefresh).not.toHaveBeenCalled();
    // The focused tab is still live.
    index.onRepoChanged("/focused");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(maybeRefresh).toHaveBeenCalledExactlyOnceWith("/focused", true);
    index.setScope({ activeKey: "/background", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(maybeRefresh).toHaveBeenLastCalledWith("/background", true);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    // Closing a tab forgets what it was owed.
    index.onRepoChanged("/focused");
    index.setScope({ activeKey: "/background", retainedKeys: ["/background"], visible: true });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    index.reset();
  });
  beforeEach(() => {
    vi.useFakeTimers();
    warnings.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("re-queues a probe deferred under load with backoff instead of settling it", async () => {
    const shed = outcome("skip_deferred", {
      facts: { available: false, is_fresh: false, schema_ok: false, already_building: false },
      reason: "devmap deferred under load after 0.000s: background work is shed while the git spawn rate budget is low",
    });
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => shed);
    // The base backoff, unspread: the spread has its own test below.
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh, random: () => 0 });
    index.setScope({ activeKey: "/repo", retainedKeys: ["/repo"], visible: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    // Waiting, not settled: "skipped" is what used to strand it for the session.
    expect(index.get("/repo").phase).toBe("scheduled");
    expect(index.get("/repo").decision).toBe("skip_deferred");
    // Never straight back into the window that refused it.
    await vi.advanceTimersByTimeAsync(2_900);
    expect(maybeRefresh).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(200);
    expect(maybeRefresh).toHaveBeenCalledTimes(2);
    // Load falls: the next attempt heals it, still as a status-only probe.
    maybeRefresh.mockResolvedValue(outcome("refresh"));
    await vi.advanceTimersByTimeAsync(6_100);
    expect(maybeRefresh).toHaveBeenCalledTimes(3);
    expect(maybeRefresh.mock.calls.every(([, changed]) => changed === false)).toBe(true);
    expect(index.get("/repo").phase).toBe("ready");
    index.reset();
  });

  it("spreads the retries of repositories one storm deferred, never shortening one", async () => {
    // Every tab restored at launch is deferred by the same storm. Asked back
    // on one schedule, they met the same empty budget together again.
    const shed = outcome("skip_deferred", {
      facts: { available: false, is_fresh: false, schema_ok: false, already_building: false },
      reason: "devmap deferred under load after 0.000s: background work is shed while the git spawn rate budget is low",
    });
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => shed);
    const draws = [0, 0.5, 1];
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh, random: () => draws.shift() ?? 0 });
    const tabs = ["/a", "/b", "/c"];
    index.setScope({ activeKey: "/a", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(2_500);
    expect(maybeRefresh).toHaveBeenCalledTimes(3);
    const waits = tabs.map((tab) => index.get(tab).reason?.match(/retrying in (\d+) s/)?.[1]);
    // 3 s, 3.75 s and 4.5 s: the base backoff, lengthened by each draw.
    expect([...waits].sort()).toEqual(["3", "4", "5"]);
    index.reset();
  });

  it("keeps a deferred watcher tick dirty across retries and gives up after the bound", async () => {
    const shed = outcome("skip_deferred", {
      facts: { available: false, is_fresh: false, schema_ok: false, already_building: false },
      reason: "devmap deferred under load after 2.000s: the git spawn rate limit admitted nothing sooner",
    });
    const maybeRefresh = vi.fn(async (_repo: string, _changed: boolean) => shed);
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(10 * 60_000);
    // One attempt plus MAX_DEFERRED_RETRIES (5) retries, then it stops asking.
    expect(maybeRefresh).toHaveBeenCalledTimes(6);
    expect(maybeRefresh.mock.calls.every(([, changed]) => changed === true)).toBe(true);
    const settled = index.get("/repo");
    expect(settled.phase).toBe("failed");
    expect(settled.reason).toContain("deferred under load");
    expect(vi.getTimerCount()).toBe(0);
    // A new change is a new request.
    maybeRefresh.mockResolvedValue(outcome("refresh"));
    index.onRepoChanged("/repo");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(index.get("/repo").phase).toBe("ready");
    index.reset();
  });

  it("focusing a tab that gave up under load asks again; re-applying the same scope does not", async () => {
    const shed = outcome("skip_deferred", {
      facts: { available: false, is_fresh: false, schema_ok: false, already_building: false },
      reason: "devmap deferred under load after 0.000s: background work is shed while the git spawn rate budget is low",
    });
    const maybeRefresh = vi.fn(async (repo: string, _changed: boolean) => (repo === "/restored" ? shed : outcome("skip_fresh")));
    const index = createLiveIndex({ debounceMs: 0, maybeRefresh });
    const tabs = ["/focused", "/restored"];
    index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(10 * 60_000);
    expect(index.get("/restored").phase).toBe("failed");
    const restoredCalls = () => maybeRefresh.mock.calls.filter(([repo]) => repo === "/restored").length;
    expect(restoredCalls()).toBe(6);
    // An identical apply is not a request; this is the 1 Hz loop the
    // `offered` set exists to prevent.
    for (let i = 0; i < 8; i++) index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(60_000);
    expect(restoredCalls()).toBe(6);
    // Focusing the given-up tab is.
    maybeRefresh.mockImplementation(async () => outcome("refresh"));
    index.setScope({ activeKey: "/restored", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(restoredCalls()).toBe(7);
    expect(index.get("/restored").phase).toBe("ready");
    // A healthy tab is not re-asked just because focus returns to it.
    const focusedCalls = maybeRefresh.mock.calls.filter(([repo]) => repo === "/focused").length;
    index.setScope({ activeKey: "/focused", retainedKeys: tabs, visible: true });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(maybeRefresh.mock.calls.filter(([repo]) => repo === "/focused")).toHaveLength(focusedCalls);
    index.reset();
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
    const maybeRefresh = vi.fn(async (_repoPath: string, _changedOnDisk: boolean) => outcome("refresh"));
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
