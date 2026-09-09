/**
 * Live index: incremental `devmap` refresh off watcher `repo-changed`.
 *
 * Mirrors the metric freshness pattern — debounce change storms, one in-flight
 * attempt globally, surface state for the Map status strip. The Rust gate
 * (`decide_live_refresh`) owns stale→refresh / fresh→skip / in-flight→skip;
 * this module only schedules and publishes outcomes.
 */

import { writable } from "svelte/store";
import { createPacedQueue, type BackgroundScope } from "../async/pacedQueue";
import { diagnostics } from "../diagnostics/diagnostics";
import { maybeRefreshDevmap } from "./client";
import type { LiveRefreshDecision, LiveRefreshOutcome } from "./types";

/** Coalesce watcher ticks the same way `handleRepoChanged` does (200ms). */
export const LIVE_INDEX_DEBOUNCE_MS = 200;

/** Busy writers get at most 30 paced retries before requiring a new request. */
export const LIVE_INDEX_BUSY_RETRIES = 30;

export type LiveIndexPhase = "idle" | "scheduled" | "running" | "ready" | "skipped" | "failed";

export interface LiveIndexSnapshot {
  phase: LiveIndexPhase;
  decision: LiveRefreshDecision | null;
  reason: string | null;
  /** Epoch ms of the last completed attempt (refresh or skip). */
  updatedAt: number | null;
  /** Successful index publications; independent of wall-clock precision. */
  revision: number;
  /** True while a refresh child is expected to be running. */
  refreshing: boolean;
}

const EMPTY: LiveIndexSnapshot = {
  phase: "idle",
  decision: null,
  reason: null,
  updatedAt: null,
  revision: 0,
  refreshing: false,
};

export interface LiveIndexController {
  /** Svelte store of per-repo snapshots. */
  readonly snapshots: ReturnType<typeof writable<Record<string, LiveIndexSnapshot>>>;
  /** Schedule a maybe-refresh after a watcher event for `repoPath`. */
  onRepoChanged(repoPath: string): void;
  /** Snapshot for one repo, or idle defaults. */
  get(repoPath: string): LiveIndexSnapshot;
  /** Drop timers and forget state (tests / teardown). */
  reset(): void;
  setScope(scope: BackgroundScope): void;
}

function snapshotFor(
  map: Record<string, LiveIndexSnapshot>,
  repoPath: string,
): LiveIndexSnapshot {
  return map[repoPath] ?? EMPTY;
}

/**
 * Create a live-index controller. Inject `maybeRefresh` in tests.
 */
export function createLiveIndex(opts?: {
  debounceMs?: number;
  maybeRefresh?: (repoPath: string, repoChanged: boolean) => Promise<LiveRefreshOutcome>;
  scope?: BackgroundScope;
}): LiveIndexController {
  const debounceMs = opts?.debounceMs ?? LIVE_INDEX_DEBOUNCE_MS;
  const maybeRefresh = opts?.maybeRefresh ?? maybeRefreshDevmap;
  const snapshots = writable<Record<string, LiveIndexSnapshot>>({});
  const retained = new Set<string>();
  const busyRetries = new Map<string, number>();
  let revision = 0;
  const queue = createPacedQueue({
    debounceMs,
    maxWaitMs: Math.max(1_000, debounceMs),
    restMs: 1_000,
    capacity: 64,
    scope: opts?.scope,
    run,
    onError: (repoPath, error) => {
      busyRetries.delete(repoPath);
      patch(repoPath, {
        phase: "failed", decision: null,
        reason: error instanceof Error ? error.message : String(error),
        refreshing: false, updatedAt: Date.now(),
      });
    },
    onOverflow: () => diagnostics.warn("code-index", "Background index queue is full (64 repositories); additional repositories were not refreshed."),
  });

  function patch(repoPath: string, next: Partial<LiveIndexSnapshot>) {
    snapshots.update((map) => {
      const prev = snapshotFor(map, repoPath);
      return { ...map, [repoPath]: { ...prev, ...next } };
    });
  }

  async function run(repoPath: string, isCurrent: () => boolean) {
    patch(repoPath, { phase: "running", refreshing: true, reason: null });
    const outcome = await maybeRefresh(repoPath, true);
    if (!isCurrent()) return;
    if (outcome.decision === "skip_building") {
      const retries = busyRetries.get(repoPath) ?? 0;
      const retrying = retries < LIVE_INDEX_BUSY_RETRIES && queue.enqueue(repoPath);
      if (retrying) busyRetries.set(repoPath, retries + 1);
      else busyRetries.delete(repoPath);
      patch(repoPath, {
        phase: retrying ? "scheduled" : "failed",
        decision: outcome.decision,
        reason: retrying ? "Index writer is busy; refresh will retry."
          : "Index writer stayed busy or the refresh queue is full. Refresh the map again.",
        refreshing: false,
        updatedAt: Date.now(),
      });
      return;
    }
    busyRetries.delete(repoPath);
    const failed =
      outcome.decision === "refresh" && outcome.build?.ok !== true;
    patch(repoPath, {
      phase: queue.isPending(repoPath) ? "scheduled" : failed
        ? "failed"
        : outcome.decision === "refresh" ? "ready" : "skipped",
      decision: outcome.decision,
      reason: failed && !outcome.build ? "Refresh returned no build outcome" : outcome.reason,
      refreshing: false,
      updatedAt: Date.now(),
      ...(outcome.decision === "refresh" && !failed ? { revision: ++revision } : {}),
    });
  }

  return {
    snapshots,
    setScope(scope) {
      queue.setScope(scope);
      const open = new Set(scope.retainedKeys);
      snapshots.update((map) => {
        const closed = Object.keys(map).filter((key) => !open.has(key));
        if (!closed.length) return map;
        const next = { ...map };
        for (const key of closed) { delete next[key]; retained.delete(key); busyRetries.delete(key); }
        return next;
      });
    },
    onRepoChanged(repoPath: string) {
      if (!queue.enqueue(repoPath)) return;
      retained.delete(repoPath);
      retained.add(repoPath);
      // Retain queued/running states; evict only settled states. At most 64
      // pending plus one running entry can remain pinned by the scheduler.
      for (const key of retained) {
        if (retained.size <= 65) break;
        if (queue.has(key)) continue;
        retained.delete(key);
        busyRetries.delete(key);
        snapshots.update((map) => {
          const next = { ...map };
          delete next[key];
          return next;
        });
      }
      snapshots.update((map) => {
        const prev = snapshotFor(map, repoPath);
        if (prev.phase === "running" || prev.phase === "scheduled") return map;
        return { ...map, [repoPath]: { ...prev, phase: "scheduled" } };
      });
    },
    get(repoPath: string) {
      let current = EMPTY;
      snapshots.subscribe((map) => {
        current = snapshotFor(map, repoPath);
      })();
      return current;
    },
    reset() {
      queue.reset();
      retained.clear();
      busyRetries.clear();
      snapshots.set({});
    },
  };
}

/** App-wide live index — wired from `App.svelte` on `repo-changed`. */
export const liveIndex = createLiveIndex({
  scope: { activeKey: null, retainedKeys: [], visible: false },
});
