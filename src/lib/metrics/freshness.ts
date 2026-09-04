/**
 * Automatic freshness for the repository metrics: LOC, churn, coverage, storage.
 *
 * ## What this replaces
 *
 * Every metric panel used to own its own one-shot fetch, keyed on the
 * repository path changing and nothing else:
 *
 * * `PulseView` fetched LOC in a `$effect` that fired only when
 *   `currentPath !== loadedPath`, so the headline line count was whatever it
 *   had been when the tab was opened — a day of editing later, still the same
 *   number, with nothing on screen saying so.
 * * Coverage was fetched **twice**, independently, by `PulseView` and
 *   `CoverageViewer`, each with its own cache and its own idea of the truth.
 * * `StoragePanel` scanned once per path change, and otherwise only when the
 *   user pressed Rescan.
 *
 * Meanwhile the backend already emits `repo-changed` on every settled write,
 * and only `repoStore` was listening.
 *
 * A metric here is defined once, fetched at most once at a time per
 * repository however many panels want it, and revalidated when the repository
 * actually changes.
 *
 * ## The honesty rule
 *
 * A refresh that fails does **not** discard the last good value — panels would
 * blank out on a transient error. But it must never let that value read as
 * current either. So a snapshot carries `value` *and* `stale`, and the two are
 * independent: `value` is the last thing successfully measured, `stale` says
 * why it may no longer describe the repository. A caller that renders `value`
 * without consulting `stale` is displaying a measurement that did not happen.
 *
 * ## Cost awareness
 *
 * Metrics are not equally cheap. A storage scan walks the entire worktree with
 * a 20-second deadline; a coverage scan is content-fingerprinted in Rust and
 * usually returns from cache. Revalidating both on every watcher tick would
 * turn a `git checkout` into a stampede. Each metric therefore declares:
 *
 * * `debounceMs` — how long change events coalesce before one refresh runs;
 * * `minIntervalMs` — a floor between two *completed* refreshes, so a repo
 *   under continuous churn cannot pin a scan at 100% duty cycle.
 *
 * Both are per metric per repository, so a busy repository never starves a
 * quiet one.
 */

/** Why a snapshot's `value` may no longer describe the repository. */
export type StaleReason =
  /** Nothing has been measured yet. */
  | "never-measured"
  /** The repository changed after this value was measured. */
  | "repository-changed"
  /** A revalidation was attempted and failed; `value` predates it. */
  | "refresh-failed"
  /** The measurement itself reported that a budget cut it short. */
  | "partial";

export type MetricState = "idle" | "loading" | "ready" | "failed";

export interface MetricSnapshot<T> {
  state: MetricState;
  /**
   * Last value that was successfully measured, retained across a failed
   * refresh. Never render this without checking `stale`.
   */
  value: T | null;
  /** Epoch milliseconds when `value` was measured, or null when never. */
  measuredAt: number | null;
  /** Non-null whenever `value` may not describe the repository right now. */
  stale: StaleReason | null;
  /** Message from the most recent failure; cleared by the next success. */
  error: string | null;
}

export interface MetricDefinition<T> {
  /** Identifier used in diagnostics and errors. */
  name: string;
  /** Performs the measurement. Rejections become `state: "failed"`. */
  measure: (repoPath: string) => Promise<T>;
  /** Coalescing window for change events. */
  debounceMs: number;
  /** Floor between two completed refreshes of the same repository. */
  minIntervalMs: number;
  /**
   * Lets a metric declare its own result incomplete — a truncated storage scan
   * or a coverage report that hit a cap. Reported as `stale: "partial"`, since
   * a bounded sample is not a complete measurement.
   */
  isPartial?: (value: T) => boolean;
  /** Bound on how many repositories are tracked at once. */
  maxRepos?: number;
  /**
   * Renders a rejection into the message panels display.
   *
   * Injected rather than fixed because the app's formatter also *redacts*:
   * a failed command can carry a remote URL with credentials in it, and that
   * must not reach a banner or the diagnostics ring verbatim. Defaults to
   * {@link describeError}, which is safe for plain values but does no
   * redaction, so anything wired to a real backend should pass its own.
   */
  formatError?: (err: unknown) => string;
  /**
   * Called once per failed measurement, before the snapshot is published.
   *
   * This is the seam to the diagnostics log. Panels used to each call
   * `reportPanelError` in their own catch block; now that measuring happens
   * here, the reporting does too — otherwise moving the fetch would have
   * silently stopped every metric failure from being recorded.
   */
  onFailure?: (repoPath: string, message: string) => void;
}

/** Injection seam so tests drive time and timers rather than waiting. */
export interface MetricClock {
  now: () => number;
  setTimeout: (fn: () => void, ms: number) => unknown;
  clearTimeout: (handle: unknown) => void;
}

const REAL_CLOCK: MetricClock = {
  now: () => Date.now(),
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export type Unsubscribe = () => void;

export interface Metric<T> {
  readonly name: string;
  /** Current snapshot for `repoPath`. Never null; an unknown repo reads idle. */
  snapshot(repoPath: string): MetricSnapshot<T>;
  /**
   * Subscribe to `repoPath`. The callback fires immediately with the current
   * snapshot, and on every subsequent change. Subscribing also requests a
   * first measurement when there is none.
   */
  subscribe(repoPath: string, listener: (snapshot: MetricSnapshot<T>) => void): Unsubscribe;
  /**
   * Measure now, bypassing the debounce. `force` also bypasses the
   * minimum-interval floor — that is the user pressing Rescan, which must
   * always do something.
   */
  refresh(repoPath: string, options?: { force?: boolean }): Promise<void>;
  /**
   * The repository changed. Marks the snapshot stale immediately and schedules
   * a coalesced refresh. Marking is synchronous on purpose: the UI should say
   * "out of date" the instant it is, not when the refresh lands.
   */
  invalidate(repoPath: string): void;
  /** Drops all state and pending timers for one repository. */
  forget(repoPath: string): void;
  /** Drops everything. Cancels pending timers; in-flight work is ignored. */
  dispose(): void;
  /** Repositories currently tracked. Test/diagnostic surface. */
  readonly trackedRepos: readonly string[];
}

interface Cell<T> {
  snapshot: MetricSnapshot<T>;
  listeners: Set<(snapshot: MetricSnapshot<T>) => void>;
  /** Rejects results from superseded measurements. */
  generation: number;
  inflight: Promise<void> | null;
  timer: unknown | null;
  /** Completion time of the last measurement attempt, success or failure. */
  lastAttemptAt: number | null;
  /** A change arrived while a measurement was running or throttled. */
  pending: boolean;
}

const IDLE: MetricSnapshot<never> = Object.freeze({
  state: "idle",
  value: null,
  measuredAt: null,
  stale: "never-measured",
  error: null,
});

function idleSnapshot<T>(): MetricSnapshot<T> {
  return IDLE as unknown as MetricSnapshot<T>;
}

/**
 * Renders an unknown rejection as a message.
 *
 * Deliberately never empty: an error whose message is "" reads in the UI as no
 * error at all, which is precisely the confusion this module exists to remove.
 */
export function describeError(err: unknown, metricName: string): string {
  if (err instanceof Error && err.message.trim()) return err.message;
  if (typeof err === "string" && err.trim()) return err;
  return `${metricName} measurement failed with no message`;
}

export function createMetric<T>(
  definition: MetricDefinition<T>,
  clock: MetricClock = REAL_CLOCK,
): Metric<T> {
  const maxRepos = Math.max(1, definition.maxRepos ?? 8);
  const debounceMs = Math.max(0, definition.debounceMs);
  const minIntervalMs = Math.max(0, definition.minIntervalMs);
  // Insertion-ordered, so the head is the least recently touched repository.
  const cells = new Map<string, Cell<T>>();
  let disposed = false;

  function touch(repoPath: string): Cell<T> {
    const existing = cells.get(repoPath);
    if (existing) {
      cells.delete(repoPath);
      cells.set(repoPath, existing);
      return existing;
    }
    const cell: Cell<T> = {
      snapshot: idleSnapshot<T>(),
      listeners: new Set(),
      generation: 0,
      inflight: null,
      timer: null,
      lastAttemptAt: null,
      pending: false,
    };
    cells.set(repoPath, cell);
    evictIfNeeded(repoPath);
    return cell;
  }

  /**
   * Evicts least-recently-used repositories past the bound.
   *
   * The bound is soft, and both exemptions are load-bearing:
   *
   * * A cell with listeners is never evicted — dropping a watched
   *   repository's state silently blanks a live view.
   * * `protectPath` is the cell `touch` just created. Without it, opening one
   *   more repository than the bound while every existing cell was watched
   *   evicted the *new* cell immediately — it is the only one with no
   *   listeners yet, because `subscribe` adds its listener after `touch`
   *   returns. The subscriber then attached to a cell no longer in the map:
   *   a subscription that never fires again, and a `snapshot()` that reads
   *   idle forever. Found by the randomized soak, not by a hand-written case.
   *
   * When nothing is evictable the map is allowed to exceed `maxRepos` rather
   * than destroy something in use.
   */
  function evictIfNeeded(protectPath: string): void {
    if (cells.size <= maxRepos) return;
    for (const [path, cell] of cells) {
      if (cells.size <= maxRepos) break;
      if (path === protectPath) continue;
      if (cell.listeners.size > 0 || cell.inflight) continue;
      cancelTimer(cell);
      cells.delete(path);
    }
  }

  function cancelTimer(cell: Cell<T>): void {
    if (cell.timer !== null) {
      clock.clearTimeout(cell.timer);
      cell.timer = null;
    }
  }

  function publish(repoPath: string, cell: Cell<T>, next: MetricSnapshot<T>): void {
    cell.snapshot = next;
    for (const listener of [...cell.listeners]) {
      try {
        listener(next);
      } catch {
        // One panel's render error must not stop the others from updating.
      }
    }
    void repoPath;
  }

  function staleAfterMeasure(value: T): StaleReason | null {
    return definition.isPartial?.(value) ? "partial" : null;
  }

  async function measureNow(repoPath: string, cell: Cell<T>): Promise<void> {
    const generation = ++cell.generation;
    cell.pending = false;
    cancelTimer(cell);
    publish(repoPath, cell, { ...cell.snapshot, state: "loading" });

    const run = (async () => {
      try {
        const value = await definition.measure(repoPath);
        if (disposed || generation !== cell.generation) return;
        publish(repoPath, cell, {
          state: "ready",
          value,
          measuredAt: clock.now(),
          stale: staleAfterMeasure(value),
          error: null,
        });
      } catch (err) {
        if (disposed || generation !== cell.generation) return;
        const message = definition.formatError
          ? definition.formatError(err)
          : describeError(err, definition.name);
        try {
          definition.onFailure?.(repoPath, message);
        } catch {
          // A broken diagnostics sink must not swallow the measurement result.
        }
        // Keep the last good value, but never let it read as current.
        publish(repoPath, cell, {
          state: "failed",
          value: cell.snapshot.value,
          measuredAt: cell.snapshot.measuredAt,
          stale: cell.snapshot.value === null ? "never-measured" : "refresh-failed",
          error: message,
        });
      } finally {
        if (!disposed && generation === cell.generation) {
          cell.inflight = null;
          cell.lastAttemptAt = clock.now();
          // A change that arrived mid-flight is honoured now, through the
          // normal throttled path rather than immediately.
          if (cell.pending) schedule(repoPath, cell);
        }
      }
    })();
    cell.inflight = run;
    return run;
  }

  /**
   * Schedules a coalesced refresh, respecting both the debounce window and the
   * minimum interval between completed measurements.
   */
  function schedule(repoPath: string, cell: Cell<T>): void {
    if (disposed) return;
    cell.pending = true;
    // An in-flight measurement will re-schedule from its own `finally`; a
    // second timer here would only race it.
    if (cell.inflight) return;
    const sinceLast = cell.lastAttemptAt === null ? Infinity : clock.now() - cell.lastAttemptAt;
    const throttleWait = Math.max(0, minIntervalMs - sinceLast);
    const wait = Math.max(debounceMs, throttleWait);
    cancelTimer(cell);
    cell.timer = clock.setTimeout(() => {
      cell.timer = null;
      if (disposed || !cell.pending) return;
      void measureNow(repoPath, cell);
    }, wait);
  }

  return {
    name: definition.name,

    snapshot(repoPath: string): MetricSnapshot<T> {
      return cells.get(repoPath)?.snapshot ?? idleSnapshot<T>();
    },

    subscribe(repoPath, listener): Unsubscribe {
      if (disposed) return () => {};
      const cell = touch(repoPath);
      cell.listeners.add(listener);
      // Guarded like every other delivery: a panel whose render throws must
      // not take down the caller that subscribed it.
      try {
        listener(cell.snapshot);
      } catch {
        /* ignored, exactly as in publish() */
      }
      // First subscriber for a never-measured repository starts the first
      // measurement. Later subscribers join the same one.
      if (cell.snapshot.state === "idle" && !cell.inflight) {
        void measureNow(repoPath, cell);
      }
      let live = true;
      return () => {
        if (!live) return;
        live = false;
        cell.listeners.delete(listener);
        if (cell.listeners.size === 0) cancelTimer(cell);
      };
    },

    async refresh(repoPath, options): Promise<void> {
      if (disposed) return;
      const cell = touch(repoPath);
      // Join an in-flight measurement rather than starting a second one: two
      // concurrent storage scans of the same tree is exactly the duplication
      // this module exists to remove.
      //
      // `force` is the exception, and it has to be. A forced refresh is the
      // user pressing Rescan, which means "measure the repository as it is
      // now". Joining a scan that started before their change would answer
      // with pre-change state and stamp it with a current `measuredAt` — a
      // stale reading presented as fresh, which is the one thing this module
      // must never do. The superseded measurement is discarded by the
      // generation guard when it lands.
      if (cell.inflight && !options?.force) return cell.inflight;
      if (!options?.force && cell.lastAttemptAt !== null) {
        const sinceLast = clock.now() - cell.lastAttemptAt;
        if (sinceLast < minIntervalMs) {
          schedule(repoPath, cell);
          return;
        }
      }
      return measureNow(repoPath, cell);
    },

    invalidate(repoPath): void {
      if (disposed) return;
      const cell = cells.get(repoPath);
      // Not tracked means no panel is watching and nothing has been measured;
      // creating state here would grow the map from watcher noise alone.
      if (!cell) return;
      if (cell.snapshot.value !== null && cell.snapshot.stale === null) {
        publish(repoPath, cell, { ...cell.snapshot, stale: "repository-changed" });
      }
      schedule(repoPath, cell);
    },

    forget(repoPath): void {
      const cell = cells.get(repoPath);
      if (!cell) return;
      cancelTimer(cell);
      // Bump the generation so a measurement still in flight is discarded.
      cell.generation += 1;
      cells.delete(repoPath);
    },

    dispose(): void {
      disposed = true;
      for (const cell of cells.values()) {
        cancelTimer(cell);
        cell.generation += 1;
        cell.listeners.clear();
      }
      cells.clear();
    },

    get trackedRepos(): readonly string[] {
      return [...cells.keys()];
    },
  };
}

/**
 * A set of metrics that revalidate together when a repository changes.
 *
 * The registry is what the `repo-changed` listener talks to: one call, and
 * every metric decides for itself — by its own debounce and cost floor —
 * when to actually re-measure.
 */
export interface MetricRegistry {
  register(metric: Metric<unknown>): void;
  /** Route a `repo-changed` event to every registered metric. */
  invalidate(repoPath: string): void;
  /** A repository was closed: drop its state everywhere. */
  forget(repoPath: string): void;
  dispose(): void;
  readonly metrics: readonly Metric<unknown>[];
}

export function createMetricRegistry(): MetricRegistry {
  const metrics: Metric<unknown>[] = [];
  return {
    register(metric) {
      if (!metrics.includes(metric)) metrics.push(metric);
    },
    invalidate(repoPath) {
      for (const metric of metrics) metric.invalidate(repoPath);
    },
    forget(repoPath) {
      for (const metric of metrics) metric.forget(repoPath);
    },
    dispose() {
      for (const metric of metrics) metric.dispose();
      metrics.length = 0;
    },
    get metrics() {
      return metrics;
    },
  };
}

/**
 * Human-readable staleness, for panels that show a freshness line.
 *
 * Returns null when the snapshot is genuinely current, so a caller can render
 * nothing at all rather than a reassuring "up to date" badge that would have
 * to be trusted.
 */
export function describeStaleness(
  snapshot: MetricSnapshot<unknown>,
  now: number,
): string | null {
  if (snapshot.stale === null) return null;
  switch (snapshot.stale) {
    case "never-measured":
      return "not measured yet";
    case "repository-changed":
      return "the repository changed since this was measured";
    case "refresh-failed":
      return snapshot.measuredAt === null
        ? "the last refresh failed"
        : `the last refresh failed — showing the value from ${formatAge(now - snapshot.measuredAt)} ago`;
    case "partial":
      return "a scan limit cut this measurement short, so it is a floor, not a total";
  }
}

/** Compact age rendering: "12s", "4m", "2h", "3d". */
export function formatAge(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}
