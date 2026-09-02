/**
 * The Work view: tasks × worktrees × pull requests × runs × verdicts, joined.
 *
 * A pure projection over data five other panes already fetch. Nothing here
 * calls out, so the join — which is the whole feature — is testable without a
 * repository, a harness, or a network.
 *
 * # Absence is a state, and so is "we could not look"
 *
 * Each source can be present, empty, or unreadable, and a row assembled from
 * an unreadable source looks exactly like a row assembled from an empty one:
 * a task with no pull requests renders identically to a task whose pull
 * requests could not be fetched. `WorkSources` is what keeps those apart, and
 * the view is required to render it — an unreadable source is not a footnote,
 * it is the reason the screen in front of you is incomplete.
 */

import type { Grant } from "../grants/types";
import type { LedgerEvent } from "../ledger/types";
import type { TaskLease } from "../tasks/types";
import type { WorktreeInfo } from "../branches/types";
import type { PullRequestInfo, WorkflowRunInfo } from "../github/types";
import type { PolicyStatus } from "../stores/harnessStore";

/** Whether one source could be consulted, and why not when it could not. */
export interface WorkSourceState {
  /** True when this source answered — including when it answered "nothing". */
  ok: boolean;
  /**
   * False when the source is simply not present here, which is the ordinary
   * case for a repository with no DevCouncil store. Distinct from `ok`: a
   * source that is absent was not consulted and did not fail.
   */
  present: boolean;
  /** Empty when `ok`; otherwise why the source could not be read. */
  detail: string;
}

export interface WorkSources {
  tasks: WorkSourceState;
  worktrees: WorkSourceState;
  github: WorkSourceState;
  ledger: WorkSourceState;
  grants: WorkSourceState;
}

/** A worktree, with the task the ledger says it is bound to. */
export interface WorktreeBinding {
  worktree: WorktreeInfo;
  /** Empty when this worktree is bound to no task. */
  taskId: string;
}

/** Policy outcomes counted over the ledger window this projection saw. */
export interface VerdictTally {
  /** Counts per status. Every status the union declares has a key. */
  byStatus: Record<PolicyStatus, number>;
  /** Rows whose verdict could not be parsed — never folded into `allowed`. */
  unparsed: number;
  /** Total ledger rows attributed to this task. */
  events: number;
}

/** One line of the Work view. */
export interface WorkRow {
  /** Empty for the catch-all row holding everything bound to no task. */
  taskId: string;
  title: string;
  status: string;
  lease: TaskLease | null;
  worktrees: WorktreeBinding[];
  pullRequests: PullRequestInfo[];
  runs: WorkflowRunInfo[];
  grants: Grant[];
  verdicts: VerdictTally;
}

export interface WorkProjection {
  rows: WorkRow[];
  sources: WorkSources;
  /**
   * True when at least one source could not be read.
   *
   * The view header reads this rather than re-deriving it, so "this screen is
   * incomplete" is stated in one place.
   */
  degraded: boolean;
}

/** Every status the policy union declares, so a tally always has all keys. */
export const ALL_STATUSES: readonly PolicyStatus[] = [
  "allowed",
  "demoted",
  "granted",
  "widened",
  "degraded",
  "warned",
  "blocked",
  "unchecked",
];

export function emptyTally(): VerdictTally {
  const byStatus = {} as Record<PolicyStatus, number>;
  for (const status of ALL_STATUSES) byStatus[status] = 0;
  return { byStatus, unparsed: 0, events: 0 };
}

/**
 * The status one ledger row's verdict recorded.
 *
 * Returns null when the row carries no verdict at all — a plain event, not a
 * judged one — and `"unchecked"` when it carries one whose status this build
 * does not recognise. A verdict we could not read must never be counted as an
 * allow, which is what any `?? "allowed"` fallback would do.
 */
export function verdictStatus(event: LedgerEvent): PolicyStatus | "unparsed" | null {
  if (!event.verdict_json) return null;
  try {
    const parsed = JSON.parse(event.verdict_json) as { status?: unknown };
    const status = parsed?.status;
    if (typeof status === "string" && (ALL_STATUSES as readonly string[]).includes(status)) {
      return status as PolicyStatus;
    }
    return "unparsed";
  } catch {
    return "unparsed";
  }
}

export interface WorkInputs {
  /** Active leases, or null when the task store could not be read. */
  leases: readonly TaskLease[] | null;
  /** Task scope titles keyed by id, for rows a lease does not name. */
  titles: Readonly<Record<string, string>>;
  worktrees: readonly WorktreeInfo[] | null;
  /** Worktree path → task id, from the ledger's binding events. */
  bindings: Readonly<Record<string, string>>;
  pullRequests: readonly PullRequestInfo[] | null;
  runs: readonly WorkflowRunInfo[] | null;
  /** The ledger tail this projection saw. Null when it could not be read. */
  events: readonly LedgerEvent[] | null;
  grants: readonly Grant[] | null;
  sources: WorkSources;
}

/** The row every unbound worktree, PR and run falls into. */
export const UNBOUND_ROW_ID = "";

function branchOf(worktree: WorktreeInfo): string {
  return worktree.branch ?? "";
}

/**
 * Joins every source into one row per task.
 *
 * The join keys, and why each is the one it is:
 *
 * * **worktree → task** comes from the ledger's binding events, not from the
 *   branch name. A binding is a recorded decision; a branch name is a
 *   coincidence, and two worktrees can hold the same branch.
 * * **pull request → task** goes through the worktree's branch, because that
 *   is the only link GitHub knows about. A PR whose head matches no worktree
 *   branch is unbound rather than guessed at.
 * * **run → task** joins the same way, on `head_branch`.
 * * **verdict → task** comes from the ledger row's own `task_id`, which the
 *   gate recorded at the moment it judged. Nothing is inferred.
 * * **grant → task** comes from `scope.task_id`, for the same reason.
 *
 * Rows are ordered by how much is happening on them — a task with a lease and
 * an open PR above one with neither — and the unbound row is always last, so
 * it never displaces real work at the top of the screen.
 */
export function projectWork(input: WorkInputs): WorkProjection {
  const rows = new Map<string, WorkRow>();

  function rowFor(taskId: string): WorkRow {
    const existing = rows.get(taskId);
    if (existing) return existing;
    const created: WorkRow = {
      taskId,
      title: input.titles[taskId] ?? "",
      status: "",
      lease: null,
      worktrees: [],
      pullRequests: [],
      runs: [],
      grants: [],
      verdicts: emptyTally(),
    };
    rows.set(taskId, created);
    return created;
  }

  for (const lease of input.leases ?? []) {
    const row = rowFor(lease.task_id);
    row.lease = lease;
    row.status = lease.status;
  }

  // Branch → the tasks its worktrees are bound to. A branch checked out in two
  // worktrees bound to different tasks belongs to both, and a PR on it is
  // shown on both rather than arbitrarily assigned to one.
  const tasksByBranch = new Map<string, Set<string>>();
  for (const worktree of input.worktrees ?? []) {
    const taskId = input.bindings[worktree.path] ?? UNBOUND_ROW_ID;
    rowFor(taskId).worktrees.push({ worktree, taskId });
    const branch = branchOf(worktree);
    if (branch.length === 0) continue;
    const set = tasksByBranch.get(branch) ?? new Set<string>();
    set.add(taskId);
    tasksByBranch.set(branch, set);
  }

  for (const pr of input.pullRequests ?? []) {
    const owners = tasksByBranch.get(pr.head_ref);
    if (owners) {
      for (const taskId of owners) rowFor(taskId).pullRequests.push(pr);
    } else {
      rowFor(UNBOUND_ROW_ID).pullRequests.push(pr);
    }
  }

  for (const run of input.runs ?? []) {
    const owners = tasksByBranch.get(run.head_branch);
    if (owners) {
      for (const taskId of owners) rowFor(taskId).runs.push(run);
    } else {
      rowFor(UNBOUND_ROW_ID).runs.push(run);
    }
  }

  for (const grant of input.grants ?? []) {
    rowFor(grant.scope.task_id || UNBOUND_ROW_ID).grants.push(grant);
  }

  for (const event of input.events ?? []) {
    const row = rowFor(event.task_id || UNBOUND_ROW_ID);
    row.verdicts.events += 1;
    const status = verdictStatus(event);
    if (status === null) continue;
    if (status === "unparsed") {
      row.verdicts.unparsed += 1;
      continue;
    }
    row.verdicts.byStatus[status] += 1;
  }

  const ordered = [...rows.values()].sort(compareRows);
  return {
    rows: ordered,
    sources: input.sources,
    degraded: Object.values(input.sources).some((s: WorkSourceState) => !s.ok),
  };
}

/** How much is going on, for ordering. Never a claim about importance. */
function weight(row: WorkRow): number {
  return (
    (row.lease ? 8 : 0) +
    row.pullRequests.length * 4 +
    row.worktrees.length * 2 +
    row.runs.length +
    row.grants.length
  );
}

function compareRows(a: WorkRow, b: WorkRow): number {
  // The unbound row is last whatever it holds: it is a bucket, not a task, and
  // in a repository that has just started binding it holds everything.
  if (a.taskId === UNBOUND_ROW_ID) return 1;
  if (b.taskId === UNBOUND_ROW_ID) return -1;
  const byWeight = weight(b) - weight(a);
  if (byWeight !== 0) return byWeight;
  return a.taskId.localeCompare(b.taskId);
}

/**
 * Statuses worth colouring in a dense tally.
 *
 * `allowed` is the overwhelming majority of every ledger, so showing it as a
 * chip beside the exceptions would bury them. It stays in the total.
 */
export function noteworthyStatuses(tally: VerdictTally): [PolicyStatus, number][] {
  return ALL_STATUSES.filter((s) => s !== "allowed" && tally.byStatus[s] > 0).map((s) => [
    s,
    tally.byStatus[s],
  ]);
}

/** One sentence naming what could not be read, or empty when all of it could. */
export function degradedSummary(sources: WorkSources): string {
  const failed = Object.entries(sources)
    .filter(([, state]) => !(state as WorkSourceState).ok)
    .map(([name, state]) => `${name} (${(state as WorkSourceState).detail || "no reason given"})`);
  if (failed.length === 0) return "";
  return `This screen is incomplete — could not read ${failed.join(", ")}.`;
}
