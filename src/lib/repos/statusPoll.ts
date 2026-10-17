/**
 * Work-tree freshness for agent-driven sessions.
 *
 * The `.git` watcher catches commits, stashes and checkouts, but an agent
 * editing files only changes the working tree — nothing under `.git` moves
 * until something stages. A light `git status` poll closes that gap: one fast
 * subprocess per tick, skipped whenever the window is hidden, a load is
 * already running, or the previous poll has not landed yet.
 */

import { CADENCE_CEILING_MS } from "../runtime/loadCadence";
import type { RepoChange } from "./events";

export const STATUS_POLL_INTERVAL_MS = 6_000;
/**
 * How many times its own cost a poll waits before the next one. `git status`
 * on a large work tree took 4 s on average (9.4 s at worst), and a fixed 6 s
 * rest after it kept git running about 40% of the time on the one repository
 * someone was looking at. Ten times the last duration holds the duty cycle
 * under 10% whatever the repository costs.
 */
export const STATUS_POLL_COST_FACTOR = 10;
/** No rest is ever longer than the cadence ceiling, however slow the last poll was. */
export const STATUS_POLL_MAX_INTERVAL_MS = CADENCE_CEILING_MS;

export interface PollRestInput {
  /** How long the last poll's `git status` took; null before the first. */
  lastDurationMs: number | null;
}

/**
 * The rest between one poll finishing and the next starting, before the
 * event-loop stretch `decideCadence` adds on top: never shorter than
 * {@link STATUS_POLL_INTERVAL_MS}, and never shorter than
 * {@link STATUS_POLL_COST_FACTOR} times what the last poll cost.
 *
 * There is deliberately NO longer rest for a repository that looks quiet. The
 * worktree watch is non-recursive (`watcher/debouncer.rs`), so an unstaged
 * edit to a nested file writes nothing under `.git` and raises no event: this
 * poll is the primary detector of ordinary edits, and a quiet minute must not
 * make the first edit after it up to a minute late.
 */
export function statusPollRestMs(input: PollRestInput): number {
  const base = STATUS_POLL_INTERVAL_MS;
  const cost =
    input.lastDurationMs !== null && Number.isFinite(input.lastDurationMs) && input.lastDurationMs > 0
      ? input.lastDurationMs * STATUS_POLL_COST_FACTOR
      : 0;
  return Math.min(STATUS_POLL_MAX_INTERVAL_MS, Math.max(base, cost));
}

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

const MAX_POLLED_PATHS = 64;
const MARKDOWN_EXT = /\.(md|markdown|mdown|mkd|mdx)$/i;

/**
 * The change a status poll observed, in the watcher's vocabulary, or null when
 * the two lists are the same.
 *
 * The worktree watch is non-recursive, so an edit to `docs/guide.md` reaches
 * no `repo-changed` event and the code index and document vault stayed stale
 * until something touched the index. The poll already reads the whole
 * worktree's status, so it names what moved: every row that appeared,
 * disappeared or changed. An entry that is, or may contain, a document (a
 * Markdown name, or no extension) also marks `documents`, as the watcher does.
 */
export function changeFromStatusPoll(
  before: readonly StatusLike[],
  after: readonly StatusLike[],
): RepoChange | null {
  const prior = new Map(before.map((row) => [row.path, row]));
  const touched: string[] = [];
  for (const row of after) {
    const old = prior.get(row.path);
    prior.delete(row.path);
    if (old === undefined || !statusEqual(old, row)) touched.push(row.path);
  }
  touched.push(...prior.keys());
  if (touched.length === 0) return null;
  const mayHoldDocuments = touched.some(
    (path) => MARKDOWN_EXT.test(path) || path.endsWith("/") || !/\.[^/]*$/.test(path),
  );
  return {
    kinds: mayHoldDocuments ? ["worktree", "documents"] : ["worktree"],
    paths: touched.slice(0, MAX_POLLED_PATHS),
    paths_truncated: touched.length > MAX_POLLED_PATHS,
  };
}
