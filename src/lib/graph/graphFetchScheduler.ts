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

import { isRefScope, type RefScope } from "./refScope";

/** The scope a request without one asks for. */
const DEFAULT_REF_SCOPE: RefScope = "named";

/** Unit separator for composite keys — invisible, untypeable, stable. */
const KEY_SEP = "\u241f";

export interface GraphFetchRequest {
  path: string | null;
  query: string;
  revision: string | null;
  /**
   * Which refs the backend walks. Part of the request identity, not a render
   * option: the same repository at the same query answers with a different
   * set of rows under each scope. Optional so a caller that does not care
   * (and every existing test) keeps the named default.
   */
  refScope?: RefScope;
}

export interface ScheduledLoad {
  path: string;
  query: string;
  revision: string | null;
  refScope: RefScope;
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
 * Canonical form of a filter query for the backend: tokens joined by one
 * space. Every query term runs in `cmd_get_commit_graph` — the backend owns
 * the filter language and rewrites history so a filtered graph stays
 * connected — so this is the ONE place that decides when two queries are
 * the same request. The scheduler key and the store's cache both use it,
 * and a stray space never re-walks history.
 */
export function normalizeGraphQuery(query: string): string {
  return query.trim().split(/\s+/).filter(Boolean).join(" ");
}

/**
 * Identity of a graph request: path, revision, ref scope, and the normalized
 * query. A different query is a different graph — the backend applies every
 * term (author, sha, type, free text, path) — so each filter edit re-arms the
 * debounce window and fires its own fetch once typing settles. The ref scope
 * belongs here for the same reason and no other: without it, changing which
 * refs the graph walks left the key unchanged, the scheduler treated the
 * request as already fired, and the setting did nothing at all until some
 * unrelated event happened to reload the pane.
 */
export function graphRequestKey(req: GraphFetchRequest): string {
  const scope = isRefScope(req.refScope) ? req.refScope : DEFAULT_REF_SCOPE;
  const base = `${req.path ?? ""}${KEY_SEP}${req.revision ?? ""}${KEY_SEP}${scope}`;
  const query = normalizeGraphQuery(req.query);
  return query ? `${base}${KEY_SEP}${query}` : base;
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
      pendingReq = {
        path: req.path,
        query: normalizeGraphQuery(req.query),
        revision: req.revision,
        // Normalized here, once, so a scope that never reached the key can
        // never reach the load either.
        refScope: isRefScope(req.refScope) ? req.refScope : DEFAULT_REF_SCOPE,
      };
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
