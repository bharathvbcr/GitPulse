import { queryNeedsServerFetch } from "../filter/parseQuery";

/**
 * Re-exported here because the scheduler owns the "which queries reach the
 * backend" contract: refresh()-style reloads must ask this before handing a
 * query to cmd_get_commit_graph, and importing from the scheduler seam keeps
 * that decision next to graphRequestKey instead of duplicated from parseQuery.
 */
export { queryNeedsServerFetch };

/**
 * Fetch scheduling for the commit graph pane.
 *
 * The pane's fetch used to live inline in an App-level `$effect` guarded by a
 * "last fetched key" memo. That shape had a fatal interaction with Svelte 5
 * re-run semantics: repoStore emits a fresh state object on every publish
 * (hydrate completion, branch-stats batches, the ~6 s status poll), each
 * emission tears the effect down, teardown cleared the armed 200 ms fetch
 * timer, and the body's memo check (`key === lastGraphKey`) then skipped
 * rescheduling because the key had been recorded before the debounced work
 * ran. On a freshly opened repository those emissions land well inside the
 * first window, so the one fetch that mattered was silently dropped and the
 * pane sat on its loader until some unrelated trigger forced a load.
 *
 * The contract here makes the armed timer immune to that churn:
 * - re-invoking `sync` with the request ALREADY armed is a no-op — the timer
 *   keeps its original deadline and fires on schedule;
 * - only a genuinely different key disarms and re-arms (trailing debounce for
 *   typing filters / switching branches);
 * - an already-fired key is never re-fetched until something resets the
 *   scheduler (repo closed, remount), matching the old memo's intent.
 */

export const GRAPH_FETCH_DEBOUNCE_MS = 200;

/** Unit separator for composite keys — invisible, untypeable, stable. */
const KEY_SEP = "\u241f";

export interface GraphFetchRequest {
  path: string | null;
  query: string;
  revision: string | null;
}

export interface ScheduledLoad {
  path: string;
  query: string;
  revision: string | null;
}

export type SetTimeoutFn = (fn: () => void, ms: number) => unknown;
export type ClearTimeoutFn = (handle: unknown) => void;

export interface GraphFetchSchedulerOptions {
  /** Called once per settled request, after the debounce window. */
  load: (req: ScheduledLoad) => void;
  debounceMs?: number;
  setTimeoutFn?: SetTimeoutFn;
  clearTimeoutFn?: ClearTimeoutFn;
}

/**
 * Identity of a graph request. Mirrors the historical key exactly: only
 * `path:`-style filters need git to walk history server-side, so every other
 * query edit keeps the key stable (client-side filtering owns those rows) and
 * must not re-walk the repository per keystroke.
 */
export function graphRequestKey(req: GraphFetchRequest): string {
  const base = `${req.path ?? ""}${KEY_SEP}${req.revision ?? ""}`;
  return queryNeedsServerFetch(req.query) ? `${base}${KEY_SEP}${req.query}` : base;
}

export interface GraphFetchScheduler {
  /**
   * Presents the latest desired request. Safe to call on every store
   * emission: unchanged keys leave any armed timer untouched.
   */
  sync(req: GraphFetchRequest): void;
  /** Cancels pending work and forgets fired history (unmount, repo closed). */
  reset(): void;
  /** True while a fetch is scheduled but has not fired yet. */
  readonly armed: boolean;
}

export function createGraphFetchScheduler(
  options: GraphFetchSchedulerOptions
): GraphFetchScheduler {
  const debounceMs = options.debounceMs ?? GRAPH_FETCH_DEBOUNCE_MS;
  // Default adapters adapt the ambient timer types to the scheduler's opaque
  // handle; injected fakes (tests) pass their own job objects straight through.
  const setTimeoutFn: SetTimeoutFn =
    options.setTimeoutFn ?? ((fn, ms) => setTimeout(fn, ms));
  const clearTimeoutFn: ClearTimeoutFn =
    options.clearTimeoutFn ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));

  let handle: unknown = null;
  let armedKey: string | null = null;
  let pendingReq: ScheduledLoad | null = null;
  let lastFiredKey: string | null = null;

  function disarm() {
    if (handle !== null) {
      clearTimeoutFn(handle);
      handle = null;
    }
  }

  function fire() {
    handle = null;
    const key = armedKey;
    const req = pendingReq;
    armedKey = null;
    pendingReq = null;
    lastFiredKey = key;
    if (req) options.load(req);
  }

  return {
    sync(req: GraphFetchRequest) {
      if (!req.path) {
        this.reset();
        return;
      }
      const key = graphRequestKey(req);
      // Unrelated store emission re-presenting the armed request: keep the
      // running timer exactly as-is. This is the anti-teardown guarantee.
      if (handle !== null && key === armedKey) return;
      // Request already served and nothing newer is pending: the memo case.
      if (handle === null && key === lastFiredKey) return;
      disarm();
      armedKey = key;
      pendingReq = { path: req.path, query: req.query, revision: req.revision };
      handle = setTimeoutFn(fire, debounceMs);
    },
    reset() {
      disarm();
      armedKey = null;
      pendingReq = null;
      lastFiredKey = null;
    },
    get armed() {
      return handle !== null;
    },
  };
}
