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
  staged_additions?: number;
  staged_deletions?: number;
  unstaged_additions?: number;
  unstaged_deletions?: number;
  warnings?: string[];
}

/**
 * Absent and empty are the same fact. Rust carries `warnings` with
 * `skip_serializing_if = "Vec::is_empty"`, so a row with nothing to say omits
 * the key rather than sending `[]`, and a locally-built row may do either.
 * Treating them as different would publish on every tick for no change.
 */
function stringListEqual(a: unknown, b: unknown): boolean {
  const left = Array.isArray(a) ? a : [];
  const right = Array.isArray(b) ? b : [];
  if (left.length !== right.length) return false;
  return left.every((value, index) => value === right[index]);
}

/**
 * Whether two rows carry the same facts.
 *
 * Every own key participates, for the same reason `shallowRecordListEqual`
 * compares all of them by default: a hand-written field list is a promise to
 * remember, and this one was already broken once. `warnings` arrived from Rust
 * and the list kept comparing the other eleven fields, so a row whose churn
 * stopped being partial compared equal and never republished — the explorer
 * went on showing "counts may understate reality" for numbers that had since
 * parsed cleanly. Deriving the keys means the next field Rust adds is compared
 * without anyone noticing it needs to be.
 *
 * Array-valued fields compare by content; `!==` on them is identity, and two
 * separate invokes never share an array, which would defeat the gate entirely.
 */
function statusEqual(left: StatusLike, right: StatusLike): boolean {
  const leftRecord = left as unknown as Record<string, unknown>;
  const rightRecord = right as unknown as Record<string, unknown>;
  // The union, not one side's keys plus a count check: an omitted `warnings`
  // and an empty one are the same fact carried by a different number of keys,
  // so comparing key counts first reports a quiet repo as changed every tick.
  for (const key of new Set([...Object.keys(leftRecord), ...Object.keys(rightRecord)])) {
    const leftValue = leftRecord[key];
    const rightValue = rightRecord[key];
    if (Array.isArray(leftValue) || Array.isArray(rightValue)) {
      if (!stringListEqual(leftValue, rightValue)) return false;
    } else if (leftValue !== rightValue) {
      return false;
    }
  }
  return true;
}

/**
 * Element-wise equality over everything the wire carries. Deliberately strictly
 * index-wise — `[a, b]` vs `[b, a]` counts as DIFFERENT. The gate only skips
 * publishes, so a reorder costing one extra publish is the safe direction;
 * multiset matching could miss a duplicate-path entry flipping sides. Kept
 * generic/structural (no FileStatus import) so statusPoll stays free of
 * store-side import cycles.
 */
export function statusesEqual(a: readonly StatusLike[], b: readonly StatusLike[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (!statusEqual(a[i], b[i])) return false;
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
