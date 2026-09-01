/**
 * Work-tree freshness for agent-driven sessions.
 *
 * The `.git` watcher catches commits, stashes and checkouts, but an agent
 * editing files only changes the working tree — nothing under `.git` moves
 * until something stages. A light `git status` poll closes that gap: one fast
 * subprocess per tick, skipped whenever the window is hidden, a load is
 * already running, or the previous poll has not landed yet.
 */

export const STATUS_POLL_INTERVAL_MS = 6_000;

/**
 * Structural shape the publish gate compares; FileStatus is compatible.
 * `old_path` mirrors Rust's `Option<String>`, which serializes to `null` —
 * not absence — for a non-rename. Assignability from FileStatus is what keeps
 * this copy honest, and it is checked at every call site.
 */
export interface StatusLike {
  path: string;
  old_path?: string | null;
  status_code: string;
  is_staged: boolean;
  is_conflicted: boolean;
  additions: number;
  deletions: number;
}

/**
 * Element-wise equality over the fields the UI renders. Deliberately strictly
 * index-wise — `[a, b]` vs `[b, a]` counts as DIFFERENT. The gate only skips
 * publishes, so a reorder costing one extra publish is the safe direction;
 * multiset matching could miss a duplicate-path entry flipping sides. Kept
 * generic/structural (no FileStatus import) so statusPoll stays free of
 * store-side import cycles.
 */
export function statusesEqual(a: readonly StatusLike[], b: readonly StatusLike[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    const left = a[i];
    const right = b[i];
    if (
      left.path !== right.path ||
      left.old_path !== right.old_path ||
      left.status_code !== right.status_code ||
      left.is_staged !== right.is_staged ||
      left.is_conflicted !== right.is_conflicted ||
      left.additions !== right.additions ||
      left.deletions !== right.deletions
    ) {
      return false;
    }
  }
  return true;
}

/**
 * Field-list equality over lists of plain records. The store's publish gate
 * uses this for branches and tags the same way statusesEqual gates statuses:
 * a snapshot whose content matches live state must not republish fresh array
 * identities into every subscriber.
 *
 * With `fields` omitted every own key participates, so a backend field added
 * later is compared automatically — the safe default for a gate that skips
 * publishes. Undeclared-field narrowing is opt-in for hot paths.
 */
export function shallowRecordListEqual<T extends Record<string, unknown>>(
  a: readonly T[],
  b: readonly T[],
  fields?: readonly (keyof T)[],
): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    const left = a[i];
    const right = b[i];
    const keys = fields ?? [...new Set([...Object.keys(left), ...Object.keys(right)])];
    for (const field of keys) {
      if (left[field] !== right[field]) return false;
    }
  }
  return true;
}

export interface PollGateInput {
  /** Document.hidden — background windows must not spend subprocesses. */
  hidden: boolean;
  hasSession: boolean;
  /** A hydrate or mutation refresh is already loading this session. */
  isLoading: boolean;
  /** The previous poll's invoke has not resolved yet. */
  inflight: boolean;
}

export function shouldRunStatusPoll(input: PollGateInput): boolean {
  if (!input.hasSession || input.hidden || input.isLoading || input.inflight) {
    return false;
  }
  return true;
}
