/**
 * Live index: incremental `devmap` refresh off watcher `repo-changed`.
 *
 * Mirrors the metric freshness pattern — debounce change storms, one in-flight
 * attempt per repo, surface state for the Map status strip. The Rust gate
 * (`decide_live_refresh`) owns stale→refresh / fresh→skip / in-flight→skip;
 * this module only schedules and publishes outcomes.
 */

import { writable } from "svelte/store";
import { maybeRefreshDevmap } from "./client";
import type { LiveRefreshDecision, LiveRefreshOutcome } from "./types";

/** Coalesce watcher ticks the same way `handleRepoChanged` does (200ms). */
export const LIVE_INDEX_DEBOUNCE_MS = 200;

export type LiveIndexPhase = "idle" | "scheduled" | "running" | "ready" | "skipped" | "failed";

export interface LiveIndexSnapshot {
  phase: LiveIndexPhase;
  decision: LiveRefreshDecision | null;
  reason: string | null;
  /** Epoch ms of the last completed attempt (refresh or skip). */
  updatedAt: number | null;
  /** True while a refresh child is expected to be running. */
  refreshing: boolean;
}

const EMPTY: LiveIndexSnapshot = {
  phase: "idle",
  decision: null,
  reason: null,
  updatedAt: null,
  refreshing: false,
};

type Timer = ReturnType<typeof setTimeout>;

export interface LiveIndexController {
  /** Svelte store of per-repo snapshots. */
  readonly snapshots: ReturnType<typeof writable<Record<string, LiveIndexSnapshot>>>;
  /** Schedule a maybe-refresh after a watcher event for `repoPath`. */
  onRepoChanged(repoPath: string): void;
  /** Snapshot for one repo, or idle defaults. */
  get(repoPath: string): LiveIndexSnapshot;
  /** Drop timers and forget state (tests / teardown). */
  reset(): void;
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
}): LiveIndexController {
  const debounceMs = opts?.debounceMs ?? LIVE_INDEX_DEBOUNCE_MS;
  const maybeRefresh = opts?.maybeRefresh ?? maybeRefreshDevmap;
  const snapshots = writable<Record<string, LiveIndexSnapshot>>({});
  const timers = new Map<string, Timer>();
  const inflight = new Set<string>();

  function patch(repoPath: string, next: Partial<LiveIndexSnapshot>) {
    snapshots.update((map) => {
      const prev = snapshotFor(map, repoPath);
      return { ...map, [repoPath]: { ...prev, ...next } };
    });
  }

  async function run(repoPath: string) {
    if (inflight.has(repoPath)) {
      // A second debounce landing while the first child is still out must not
      // start another — and must not clobber the "running" strip state.
      return;
    }
    inflight.add(repoPath);
    patch(repoPath, { phase: "running", refreshing: true, reason: null });
    try {
      const outcome = await maybeRefresh(repoPath, true);
      const failed =
        outcome.decision === "refresh" && outcome.build != null && !outcome.build.ok;
      patch(repoPath, {
        phase: failed
          ? "failed"
          : outcome.decision === "refresh"
            ? "ready"
            : "skipped",
        decision: outcome.decision,
        reason: outcome.reason,
        refreshing: false,
        updatedAt: Date.now(),
      });
    } catch (err) {
      patch(repoPath, {
        phase: "failed",
        decision: null,
        reason: err instanceof Error ? err.message : String(err),
        refreshing: false,
        updatedAt: Date.now(),
      });
    } finally {
      inflight.delete(repoPath);
    }
  }

  return {
    snapshots,
    onRepoChanged(repoPath: string) {
      if (!repoPath) return;
      const existing = timers.get(repoPath);
      if (existing) clearTimeout(existing);
      // Keep "running" visible while a child is out; only mark scheduled when
      // nothing is in flight yet.
      snapshots.update((map) => {
        const prev = snapshotFor(map, repoPath);
        if (prev.phase === "running" || inflight.has(repoPath)) return map;
        return { ...map, [repoPath]: { ...prev, phase: "scheduled" } };
      });
      timers.set(
        repoPath,
        setTimeout(() => {
          timers.delete(repoPath);
          void run(repoPath);
        }, debounceMs),
      );
    },
    get(repoPath: string) {
      let current = EMPTY;
      snapshots.subscribe((map) => {
        current = snapshotFor(map, repoPath);
      })();
      return current;
    },
    reset() {
      for (const timer of timers.values()) clearTimeout(timer);
      timers.clear();
      inflight.clear();
      snapshots.set({});
    },
  };
}

/** App-wide live index — wired from `App.svelte` on `repo-changed`. */
export const liveIndex = createLiveIndex();
