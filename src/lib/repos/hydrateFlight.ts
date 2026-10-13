/**
 * One snapshot at a time per repository tab, and at most one more owed.
 *
 * A hydrate is about eight git children (`loadSnapshot`'s six reads, then the
 * branch-stats drain), and nothing stopped a second from starting beside the
 * first: the store dropped the OLDER answer when two landed, but both still
 * ran. Watcher events every few hundred milliseconds against a status that
 * takes seconds therefore stacked snapshots on one repository, each one slower
 * for the others, until the spawn budget the whole app shares was spent on
 * answers that were thrown away.
 *
 * The rule here:
 *
 * - A request while nothing runs for the key starts at once.
 * - A request while one runs does not start a second. It marks one trailing
 *   run owed and joins it; every request that arrives meanwhile joins the SAME
 *   trailing run, so a storm costs the running snapshot plus one more, never
 *   one per event. The trailing run starts the moment the running one settles
 *   (either way), and takes the arguments of the newest request, which is the
 *   one that knows the current generation and path.
 * - A run that outlives {@link HYDRATE_INFLIGHT_MAX_MS} no longer holds the
 *   key: it is hung, not slow, and a newer request starts beside it. Its late
 *   answer is the store's to discard by its own run token.
 *
 * This is the single in-flight owner. A policy that decides WHEN a refresh is
 * worth asking for (`watcherRefresh.ts`) should hand its request to this and
 * return the promise, not keep a second in-flight flag of its own: two
 * single-flights stacked on one another is how a trailing refresh gets owed
 * twice, once by each.
 *
 * Pure apart from the injected clock, so a test can drive it without a store.
 */

/**
 * How long a snapshot may run before a newer request stops waiting for it.
 * Named and valued to match `INFLIGHT_MAX_MS` in the watcher refresh policy,
 * which holds the same bound for the same reason.
 */
export const HYDRATE_INFLIGHT_MAX_MS = 60_000;

export interface HydrateFlightDeps {
  now(): number;
}

interface Entry {
  running: Promise<void> | null;
  startedAt: number;
  /** The newest epoch asked for; see `run`. */
  epoch: number;
  /** The shared promise of the one run owed after `running`; null when none is. */
  trailing: Promise<void> | null;
  /** The newest request's start function: what the trailing run will call. */
  latest: (() => Promise<void>) | null;
}

export interface HydrateFlight {
  /**
   * Runs `start` for `key`, or arranges for it to run once after the one in
   * flight.
   *
   * `epoch` is the activation epoch the store tags the answer with (its
   * session `generation`), and it decides what is the same request: the store
   * discards an answer from an older epoch, so a request from a NEWER epoch
   * does not wait behind the running one — it starts at once, replacing
   * whatever was owed after it — and a request from an older epoch than the
   * one running joins that run instead of starting another. Within an epoch
   * the rule is the one above. The returned promise settles when the run that covers THIS request
   * has settled, so a caller that awaited a refresh and then reads the result
   * reads one that began after it asked. It never rejects: a failed run has
   * already reported itself to the store.
   */
  run(key: string, epoch: number, start: () => Promise<void>): Promise<void>;
  /** The run in flight for `key`, if any, without owing another. */
  current(key: string): Promise<void> | null;
  /** Keys with a run in flight or owed, for tests and diagnostics. */
  busy(): string[];
  /** Drops everything held for `key` (its tab closed). A run already going finishes unobserved. */
  forget(key: string): void;
}

export function createHydrateFlight(deps: HydrateFlightDeps): HydrateFlight {
  const entries = new Map<string, Entry>();

  const begin = (key: string, entry: Entry, start: () => Promise<void>): Promise<void> => {
    entry.startedAt = deps.now();
    // `start` runs now, in this turn: a hydrate that yielded first would let a
    // later request's reads overtake it, which the store's run tokens exist to
    // arbitrate and should not have to.
    const run: Promise<void> = new Promise<void>((resolve) => resolve(start()))
      .catch(() => {
        /* the run reports its own failure to the store */
      })
      .finally(() => {
        if (entry.running === run) entry.running = null;
        if (entry.running === null && entry.trailing === null && entries.get(key) === entry) entries.delete(key);
      });
    entry.running = run;
    return run;
  };

  return {
    run(key, epoch, start) {
      let entry = entries.get(key);
      if (!entry) {
        entry = { running: null, startedAt: 0, epoch, trailing: null, latest: null };
        entries.set(key, entry);
      }
      const live = entry;
      if (live.running !== null && epoch < live.epoch) {
        // An older epoch's answer would be discarded; it is not worth a run.
        return live.trailing ?? live.running;
      }
      const superseding = live.running !== null && epoch > live.epoch;
      live.epoch = Math.max(live.epoch, epoch);
      const hung = live.running !== null && deps.now() - live.startedAt >= HYDRATE_INFLIGHT_MAX_MS;
      if (hung || superseding) {
        // Whatever was owed after the hung or superseded run is replaced by
        // this one: its answer is the one the store will apply.
        live.trailing = null;
        live.latest = null;
        return begin(key, live, start);
      }
      if (live.trailing !== null) {
        live.latest = start;
        return live.trailing;
      }
      if (live.running === null) return begin(key, live, start);
      live.latest = start;
      const owed: Promise<void> = live.running.then(() => {
        // The tab may have closed, or a newer request replaced a hung run,
        // while this waited.
        if (entries.get(key) !== live || live.trailing !== owed) return;
        const next = live.latest ?? start;
        live.trailing = null;
        live.latest = null;
        return begin(key, live, next);
      });
      live.trailing = owed;
      return owed;
    },
    current(key) {
      return entries.get(key)?.running ?? null;
    },
    busy() {
      return [...entries.keys()];
    },
    forget(key) {
      entries.delete(key);
    },
  };
}
