/**
 * Live index: `devmap` refresh off watcher `repo-changed` and repo activation.
 *
 * Mirrors the metric freshness pattern — debounce change storms, one in-flight
 * attempt globally, surface state for the Map status strip. The Rust gate
 * (`decide_live_refresh`) owns stale→refresh / fresh→skip / in-flight→skip;
 * this module only schedules and publishes outcomes.
 *
 * Watcher ticks pass `repoChanged: true`. Opening a tab schedules a status-only
 * heal for every retained repository, not only the focused one, so a
 * payload-obsolete map (Manvi sitting behind a DevCouncil rebuild storm) can
 * upgrade without waiting for focus. Identical `setScope` applies for keys
 * already offered must not re-enqueue: every apply used to restart the 1 Hz
 * status+build loop after a finished run.
 *
 * Duty cycle. The backend sets a per-repository wait after every live build
 * (`max(30 s, 2 x build)`, longer after a timeout) and answers a watcher tick
 * inside it with `skip_cooldown`. This half holds the repository's next ask
 * until that wait ends, so every edit inside it — watcher, status poll, or any
 * future source routed through `onRepoChanged` — joins one held ask rather than
 * becoming an IPC call of its own, and the last edit of a storm is still owed
 * one build. Watcher edits to a tab that is not focused are held until it is.
 * The backend no longer holds the IPC call for the build: it answers
 * `started`, and this half collects the outcome by asking again.
 */

import { get, writable } from "svelte/store";
import { jitteredDelayMs, jitteredRetryDelayMs, MAX_DEFERRED_RETRIES } from "../async/deferral";
import { createPacedQueue, type BackgroundScope } from "../async/pacedQueue";
import { diagnostics } from "../diagnostics/diagnostics";
import { maybeRefreshDevmap } from "./client";
import type { DevmapBuildOutcome, LiveRefreshDecision, LiveRefreshOutcome } from "./types";

/**
 * Mirror of Rust `build_is_echo_or_failure` for the *publication* half:
 * an `unchanged: true` build is a successful warm-path echo, not a new
 * index. Missing `unchanged` is not an echo — only an explicit true is.
 */
function isUnchangedEcho(build: DevmapBuildOutcome | null | undefined): boolean {
  if (!build || build.ok !== true) return false;
  const report = build.report;
  if (report === null || report === undefined || typeof report !== "object" || Array.isArray(report)) {
    return false;
  }
  return (report as { unchanged?: unknown }).unchanged === true;
}

/** Coalesce watcher ticks the same way `handleRepoChanged` does (200ms). */
export const LIVE_INDEX_DEBOUNCE_MS = 200;

/** Busy writers get at most 30 paced retries before requiring a new request. */
export const LIVE_INDEX_BUSY_RETRIES = 30;

/** Longest wait between two asks of a busy writer. */
export const LIVE_INDEX_BUSY_RETRY_MAX_MS = 30_000;

/**
 * Wait before busy retry `attempt` (1-based): 1 s, doubling, capped at
 * {@link LIVE_INDEX_BUSY_RETRY_MAX_MS}. The retries used to run at the queue's
 * fixed one-second rest, which asked the backend once a second for exactly as
 * long as a build held the writer, and gave up after ~30 s on a build that
 * routinely takes minutes. Backing off spends six asks on the first half
 * minute and stretches the same 30-retry budget across ~13 minutes.
 */
export function busyRetryDelayMs(attempt: number): number {
  const step = Number.isFinite(attempt) ? Math.max(1, Math.floor(attempt)) : 1;
  return Math.min(1_000 * 2 ** Math.min(step - 1, 30), LIVE_INDEX_BUSY_RETRY_MAX_MS);
}

/**
 * How long a build the backend started in the background is collected:
 * its 15-minute `BUILD_DEADLINE` plus a minute. Past that the child has been
 * killed and its outcome stored, so a repository still answering
 * `skip_building` is a different writer's, not ours.
 */
export const LIVE_INDEX_BUILD_WAIT_MS = 16 * 60_000;

/** Longest wait between two asks for a background build's outcome. */
export const LIVE_INDEX_COLLECT_MAX_MS = 10_000;

/**
 * Consecutive `skip_cooldown` answers a held watcher edit is re-asked through
 * before the next edit has to ask again. Each re-ask lands when the backend's
 * own wait ends, so more than one is reached only if a new wait was set in
 * between.
 */
export const LIVE_INDEX_COOLDOWN_REASKS = 3;

/** Wait before collection ask `attempt` (1-based): 1 s, doubling, capped. */
function collectDelayMs(attempt: number): number {
  return Math.min(1_000 * 2 ** Math.min(Math.max(attempt, 1) - 1, 30), LIVE_INDEX_COLLECT_MAX_MS);
}

/**
 * `paused`: the last automatic build ran past its deadline and the backend
 * paused automatic indexing; `reason` is the sentence the Map strip shows.
 */
export type LiveIndexPhase = "idle" | "scheduled" | "running" | "ready" | "skipped" | "failed" | "paused";

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
  /**
   * Stage the running build has reached, from the kernel's own `[n/5]`
   * progress. `null` whenever no build is running, and also while one is —
   * until its first stage line arrives, because claiming stage 1 before the
   * kernel says so would be a progress bar this app invented.
   */
  stage: { current: number; total: number } | null;
}

const EMPTY: LiveIndexSnapshot = {
  phase: "idle",
  decision: null,
  reason: null,
  updatedAt: null,
  revision: 0,
  refreshing: false,
  stage: null,
};

export interface LiveIndexController {
  /** Svelte store of per-repo snapshots. */
  readonly snapshots: ReturnType<typeof writable<Record<string, LiveIndexSnapshot>>>;
  /** Schedule a maybe-refresh after a watcher event for `repoPath`. */
  onRepoChanged(repoPath: string): void;
  /** Record a build stage reported by the kernel for `repoPath`. */
  onBuildProgress(repoPath: string, stage: number, total: number): void;
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
  /** Source in [0, 1) that spreads retry waits; see `jitteredDelayMs`. */
  random?: () => number;
}): LiveIndexController {
  const debounceMs = opts?.debounceMs ?? LIVE_INDEX_DEBOUNCE_MS;
  const maybeRefresh = opts?.maybeRefresh ?? maybeRefreshDevmap;
  const random = opts?.random ?? Math.random;
  const snapshots = writable<Record<string, LiveIndexSnapshot>>({});
  const retained = new Set<string>();
  const busyRetries = new Map<string, number>();
  /** Consecutive answers the backend declined to run under load, per repo. */
  const deferrals = new Map<string, number>();
  /** Keys whose pending/running attempt came from a watcher tick. */
  const dirty = new Set<string>();
  /** Retained keys already offered a status-only heal. Re-open after close. */
  const offered = new Set<string>();
  /** Focused key at the last `setScope`, so a re-apply is not a focus change. */
  let lastFocused: string | null = null;
  /**
   * Active key at the last `setScope`, visible or not: the one tab whose
   * watcher edits are live. Hiding the window does not make the others live.
   */
  let lastActive: string | null = opts?.scope?.activeKey ?? null;
  /** Retained keys at the last `setScope`; `null` until a scope is applied. */
  let retainedScope: Set<string> | null = opts?.scope ? new Set(opts.scope.retainedKeys) : null;
  /** Epoch ms until which the backend answers a watcher tick without a build. */
  const restUntil = new Map<string, number>();
  /** Background builds being collected: when they started, asks spent. */
  const awaiting = new Map<string, { since: number; asks: number }>();
  /** Consecutive `skip_cooldown` re-asks of a held watcher edit. */
  const cooldownAsks = new Map<string, number>();
  /** Unfocused tabs with a watcher edit held until they are focused. */
  const held = new Set<string>();
  let revision = 0;
  const queue = createPacedQueue({
    debounceMs,
    maxWaitMs: Math.max(1_000, debounceMs),
    restMs: 1_000,
    capacity: 64,
    scope: opts?.scope,
    runWhen: "retained",
    run,
    onError: (repoPath, error) => {
      forgetRetries(repoPath);
      patch(repoPath, {
        phase: "failed", decision: null,
        reason: error instanceof Error ? error.message : String(error),
        refreshing: false, updatedAt: Date.now(), stage: null,
      });
    },
    onOverflow: () => diagnostics.warn("code-index", "Background index queue is full (64 repositories); additional repositories were not refreshed."),
  });

  /**
   * Both retry budgets end together, and only when an attempt settles. Neither
   * a busy answer nor a deferral resets the other's count, so alternating
   * between them cannot retry forever.
   */
  function forgetRetries(repoPath: string) {
    busyRetries.delete(repoPath);
    deferrals.delete(repoPath);
    cooldownAsks.delete(repoPath);
  }

  /** Everything a closed or evicted repository was owed. */
  function forget(repoPath: string) {
    forgetRetries(repoPath);
    dirty.delete(repoPath);
    held.delete(repoPath);
    awaiting.delete(repoPath);
    restUntil.delete(repoPath);
  }

  function noteRest(repoPath: string, waitMs: number | null | undefined) {
    if (typeof waitMs === "number" && Number.isFinite(waitMs) && waitMs > 0) {
      restUntil.set(repoPath, Date.now() + waitMs);
    }
  }

  function restRemainingMs(repoPath: string): number {
    const until = restUntil.get(repoPath);
    if (until === undefined) return 0;
    const left = until - Date.now();
    if (left > 0) return left;
    restUntil.delete(repoPath);
    return 0;
  }

  /**
   * Ask about a watcher edit no sooner than the backend will act on it. Every
   * edit inside the wait joins this one held ask: `enqueue` never pulls a held
   * entry forward.
   */
  function enqueueEdit(repoPath: string): boolean {
    const waitMs = restRemainingMs(repoPath);
    return queue.enqueue(repoPath, waitMs > 0 ? { delayMs: waitMs } : undefined);
  }

  function patch(repoPath: string, next: Partial<LiveIndexSnapshot>) {
    snapshots.update((map) => {
      const prev = snapshotFor(map, repoPath);
      return { ...map, [repoPath]: { ...prev, ...next } };
    });
  }

  async function run(repoPath: string, isCurrent: () => boolean) {
    const repoChanged = dirty.has(repoPath);
    // Consumed by this ask. An edit that lands while it is in flight marks the
    // repository again, so it is owed its own build rather than being erased
    // by this ask's settlement.
    dirty.delete(repoPath);
    const collecting = awaiting.get(repoPath);
    // A collection ask keeps the stage the kernel last reported.
    patch(repoPath, collecting
      ? { phase: "running", refreshing: true }
      : { phase: "running", refreshing: true, reason: null, stage: null });
    const outcome = await maybeRefresh(repoPath, repoChanged);
    if (!isCurrent()) return;
    /** This ask's edit is still owed: put it back for the retry. */
    const stillOwed = () => {
      if (repoChanged) dirty.add(repoPath);
    };
    if (outcome.decision === "started") {
      // The build took every edit up to now. It runs off the IPC path; ask
      // again for its outcome.
      forgetRetries(repoPath);
      const collectingNow = queue.enqueue(repoPath, { delayMs: collectDelayMs(1) });
      if (collectingNow) awaiting.set(repoPath, { since: Date.now(), asks: 1 });
      patch(repoPath, {
        phase: collectingNow ? "running" : "failed",
        decision: outcome.decision,
        reason: collectingNow ? null : "The index build started, but the refresh queue is full. Refresh the map again.",
        refreshing: collectingNow,
        updatedAt: Date.now(),
      });
      return;
    }
    if (outcome.decision === "skip_building" && collecting) {
      // Our own build, still running. Bounded by the build's deadline, not by
      // the busy budget, which is sized for somebody else's writer.
      stillOwed();
      const asks = collecting.asks + 1;
      const within = Date.now() - collecting.since < LIVE_INDEX_BUILD_WAIT_MS;
      if (within && queue.enqueue(repoPath, { delayMs: collectDelayMs(asks) })) {
        awaiting.set(repoPath, { since: collecting.since, asks });
        patch(repoPath, { phase: "running", refreshing: true, updatedAt: Date.now() });
        return;
      }
      awaiting.delete(repoPath);
      patch(repoPath, {
        phase: "failed",
        decision: outcome.decision,
        reason: within
          ? "The index build is running, but the refresh queue is full. Refresh the map again."
          : "The background index build never reported back. Refresh the map again.",
        refreshing: false,
        updatedAt: Date.now(),
        stage: null,
      });
      return;
    }
    awaiting.delete(repoPath);
    if (outcome.decision === "skip_deferred") {
      // The backend's spawn gate declined to start the probe under load —
      // nothing was examined. Settling it as "skipped" is what left every tab
      // restored at launch unhealed for the session, so ask again, later.
      // The edit is kept: the retry still owes the watcher its rebuild. Every
      // tab restored at launch is deferred by the same storm, so the waits
      // are spread rather than returning to the gate on one millisecond.
      const attempt = (deferrals.get(repoPath) ?? 0) + 1;
      const delayMs = jitteredRetryDelayMs(attempt, random);
      const retrying = attempt <= MAX_DEFERRED_RETRIES && queue.enqueue(repoPath, { delayMs });
      if (retrying) {
        deferrals.set(repoPath, attempt);
        stillOwed();
      } else {
        // Bounded: a watcher tick or focusing the tab is a new request.
        // `offered` is left alone, so re-applying the same scope cannot
        // restart the chain (see `setScope`).
        forgetRetries(repoPath);
      }
      patch(repoPath, {
        phase: retrying ? "scheduled" : "failed",
        decision: outcome.decision,
        reason: retrying
          ? `Index check deferred while the app is busy; retrying in ${Math.round(delayMs / 1_000)} s.`
          : outcome.reason ?? "Index check stayed deferred under load. Refresh the map again.",
        refreshing: false,
        updatedAt: Date.now(),
        stage: null,
      });
      return;
    }
    if (outcome.decision === "skip_building") {
      const retries = busyRetries.get(repoPath) ?? 0;
      const retrying = retries < LIVE_INDEX_BUSY_RETRIES &&
        queue.enqueue(repoPath, { delayMs: jitteredDelayMs(busyRetryDelayMs(retries + 1), random) });
      if (retrying) {
        busyRetries.set(repoPath, retries + 1);
        stillOwed();
      } else forgetRetries(repoPath);
      patch(repoPath, {
        phase: retrying ? "scheduled" : "failed",
        decision: outcome.decision,
        reason: retrying ? "Index writer is busy; refresh will retry."
          : "Index writer stayed busy or the refresh queue is full. Refresh the map again.",
        refreshing: false,
        updatedAt: Date.now(),
        stage: null,
      });
      return;
    }
    if (outcome.decision === "skip_cooldown") {
      // The backend is waiting out the last build. A watcher edit inside the
      // wait is owed exactly one ask when it ends — not a busy spin, and not
      // nothing, which is what lost the last edit of every storm. An
      // activation is not owed a build.
      noteRest(repoPath, outcome.cooldown_remaining_ms);
      const waitMs = restRemainingMs(repoPath);
      const asks = (cooldownAsks.get(repoPath) ?? 0) + 1;
      busyRetries.delete(repoPath);
      deferrals.delete(repoPath);
      const reasking = repoChanged && waitMs > 0 && asks <= LIVE_INDEX_COOLDOWN_REASKS &&
        queue.enqueue(repoPath, { delayMs: waitMs });
      if (reasking) {
        cooldownAsks.set(repoPath, asks);
        stillOwed();
      } else cooldownAsks.delete(repoPath);
      patch(repoPath, {
        phase: outcome.index_too_slow === true ? "paused" : "scheduled",
        decision: outcome.decision,
        reason: outcome.reason,
        refreshing: false,
        updatedAt: Date.now(),
        stage: null,
      });
      return;
    }
    forgetRetries(repoPath);
    if (outcome.decision === "refresh") noteRest(repoPath, outcome.cooldown_remaining_ms);
    // Edits that arrived while a background build ran are not in it: they
    // are owed one build, held until the rest that build just set has ended.
    if (collecting && repoChanged && enqueueEdit(repoPath)) dirty.add(repoPath);
    const failed =
      outcome.decision === "refresh" && outcome.build?.ok !== true;
    // A build spawned because `repo_map.json` was missing rewrites the
    // artifacts from the existing generation, so the *store* reports
    // `unchanged: true` while the map the pane reads is brand new. Rust
    // re-probes the artifacts after the child exits and says so; treating that
    // as an echo left the freshly written map unread until a manual reload.
    const published =
      outcome.decision === "refresh" &&
      outcome.build?.ok === true &&
      (!isUnchangedEcho(outcome.build) || outcome.artifacts_restored === true);
    patch(repoPath, {
      phase: outcome.index_too_slow === true
        ? "paused"
        : queue.isPending(repoPath) ? "scheduled" : failed
        ? "failed"
        : outcome.decision === "refresh" ? "ready" : "skipped",
      decision: outcome.decision,
      reason: failed && !outcome.build ? "Refresh returned no build outcome" : outcome.reason,
      refreshing: false,
      updatedAt: Date.now(),
      stage: null,
      ...(published ? { revision: ++revision } : {}),
    });
  }

  return {
    snapshots,
    setScope(scope) {
      queue.setScope(scope);
      const open = new Set(scope.retainedKeys);
      for (const key of [...offered]) {
        if (!open.has(key)) offered.delete(key);
      }
      // Focusing a tab whose heal ran out of deferral retries is a new
      // request, so it is offered again. An identical apply is not: only a
      // focus *change* re-offers, or a busy app would restart the chain on
      // every apply.
      const focused = scope.visible ? scope.activeKey : null;
      if (focused && focused !== lastFocused) {
        const prev = get(snapshots)[focused];
        if (prev?.phase === "failed" && prev.decision === "skip_deferred") offered.delete(focused);
      }
      lastFocused = focused;
      lastActive = scope.activeKey;
      retainedScope = open;
      for (const key of [...held, ...awaiting.keys(), ...restUntil.keys()]) {
        if (!open.has(key)) forget(key);
      }
      snapshots.update((map) => {
        const closed = Object.keys(map).filter((key) => !open.has(key));
        if (!closed.length) return map;
        const next = { ...map };
        for (const key of closed) {
          delete next[key];
          retained.delete(key);
          forget(key);
        }
        return next;
      });
      // A tab focused with a watcher edit held for it: that edit's build is
      // owed now, still no sooner than the backend's wait allows.
      if (focused !== null && held.delete(focused) && !enqueueEdit(focused)) dirty.delete(focused);
      const scheduled: string[] = [];
      for (const key of scope.retainedKeys) {
        if (!key || offered.has(key)) continue;
        if (!queue.enqueue(key)) continue;
        offered.add(key);
        scheduled.push(key);
      }
      if (scheduled.length) {
        snapshots.update((map) => {
          let next = map;
          for (const key of scheduled) {
            const prev = snapshotFor(next, key);
            if (prev.phase === "running" || prev.phase === "scheduled") continue;
            if (next === map) next = { ...map };
            next[key] = { ...prev, phase: "scheduled" };
          }
          return next;
        });
      }
    },
    onRepoChanged(repoPath: string) {
      if (!repoPath || (retainedScope !== null && !retainedScope.has(repoPath))) return;
      // Only the focused tab is live. Another tab's edit is held, once, until
      // that tab is focused; the status poll and the watcher both land here,
      // so neither can rebuild a repository nobody is looking at.
      if (lastActive !== null && repoPath !== lastActive) held.add(repoPath);
      else if (!enqueueEdit(repoPath)) return;
      dirty.add(repoPath);
      retained.delete(repoPath);
      retained.add(repoPath);
      // Retain queued/running states; evict only settled states. At most 64
      // pending plus one running entry can remain pinned by the scheduler.
      for (const key of retained) {
        if (retained.size <= 65) break;
        if (queue.has(key)) continue;
        retained.delete(key);
        forget(key);
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
    onBuildProgress(repoPath, stage, total) {
      if (!Number.isFinite(stage) || !Number.isFinite(total) || total <= 0) return;
      snapshots.update((map) => {
        const prev = map[repoPath];
        // Only a build this controller believes is running may move the bar.
        // A stray event for a repository whose tab has closed, or for an
        // explicit Build the user pressed, must not resurrect a stale row.
        if (!prev || prev.phase !== "running") return map;
        return { ...map, [repoPath]: { ...prev, stage: { current: stage, total } } };
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
      deferrals.clear();
      dirty.clear();
      offered.clear();
      held.clear();
      awaiting.clear();
      restUntil.clear();
      cooldownAsks.clear();
      lastFocused = null;
      lastActive = null;
      snapshots.set({});
    },
  };
}

/** App-wide live index — wired from `App.svelte` on `repo-changed`. */
export const liveIndex = createLiveIndex({
  scope: { activeKey: null, retainedKeys: [], visible: false },
});
