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
import type { RepoOperation } from "../repos/operation";
import { agentKindsOn, isAgentWorktree } from "./agentWorktree";

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
  /**
   * The multi-step git operation parked in this worktree, if the probe ran
   * and found one. Null when the probe ran and the worktree is idle; a
   * missing probe is represented by this also being null, and by the load
   * marking the worktrees source degraded so the two never read the same.
   */
  operation: RepoOperation | null;
  /** False when the non-bare worktree could not be probed. */
  operationChecked?: boolean;
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

/**
 * What a row is keyed on.
 *
 * A DevCouncil store makes the task the unit of work. Without one — the
 * ordinary case for a repository driven by Claude Code or by hand — the unit
 * is the **worktree**: that is where a branch, its uncommitted changes, its
 * parked operation and its pull request actually live. Keying everything on
 * task in that case collapsed the entire repository into a single row labelled
 * "Not bound to a task", which is why the screen read as empty even when four
 * agents were mid-change.
 */
export type WorkRowKind = "task" | "worktree" | "unbound";

/** One line of the Work view. */
export interface WorkRow {
  /** Stable identity: task id, worktree path, or "" for the catch-all. */
  key: string;
  kind: WorkRowKind;
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
  /**
   * The multi-step git operation parked in this row's worktree, if any.
   *
   * A worktree stopped mid-rebase is the single most important thing this
   * screen can say about it — it is blocked, and it is blocked on a person.
   */
  operation: RepoOperation | null;
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
  /** Worktree path → the operation parked there. Absent means none/unknown. */
  operations: Readonly<Record<string, RepoOperation | null>>;
  sources: WorkSources;
}

/** The row every unbound worktree, PR and run falls into. */
export const UNBOUND_ROW_ID = "";

function branchOf(worktree: WorktreeInfo): string {
  return worktree.branch ?? "";
}

/**
 * Whether this repository has a task model to key rows on.
 *
 * True when the store answered with at least one lease or binding. An absent
 * or unreadable store falls through to worktree rows, which every repository
 * has — so the screen is never keyed on something that does not exist here.
 */
function usesTaskRows(input: WorkInputs): boolean {
  return (input.leases?.length ?? 0) > 0 || Object.keys(input.bindings).length > 0;
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
 * an open PR above one with neither — with parked operations first. Other unbound records follow named work.
 */
export function projectWork(input: WorkInputs): WorkProjection {
  const rows = new Map<string, WorkRow>();
  const byTask = usesTaskRows(input);

  function rowFor(key: string, kind: WorkRowKind): WorkRow {
    const existing = rows.get(key);
    if (existing) return existing;
    const created: WorkRow = {
      key,
      kind: key === UNBOUND_ROW_ID ? "unbound" : kind,
      taskId: kind === "task" ? key : "",
      title: kind === "task" ? (Object.hasOwn(input.titles, key) ? input.titles[key] : "") : "",
      status: "",
      lease: null,
      worktrees: [],
      pullRequests: [],
      runs: [],
      grants: [],
      verdicts: emptyTally(),
      operation: null,
    };
    rows.set(key, created);
    return created;
  }

  /** The catch-all, in either mode. */
  const unbound = () => rowFor(UNBOUND_ROW_ID, "unbound");

  for (const lease of input.leases ?? []) {
    const row = rowFor(lease.task_id, "task");
    row.lease = lease;
    row.status = lease.status;
  }

  // Branch → the row keys its worktrees belong to. A branch checked out in two
  // worktrees belongs to both, and a PR on it is shown on both rather than
  // arbitrarily assigned to one.
  const keysByBranch = new Map<string, Set<string>>();
  for (const worktree of uniqueBy(input.worktrees ?? [], (w) => w.path)) {
    const taskId = Object.hasOwn(input.bindings, worktree.path) ? input.bindings[worktree.path] : UNBOUND_ROW_ID;
    // In worktree mode the worktree IS the row, so it never lands in the
    // catch-all: a repository with no task store still shows one row per
    // place work is happening.
    const key = byTask ? taskId : worktree.path;
    const row = rowFor(key, byTask ? "task" : "worktree");
    const parked = Object.hasOwn(input.operations, worktree.path) ? input.operations[worktree.path] : null;
    row.worktrees.push({ worktree, taskId, operation: parked, operationChecked: worktree.is_bare || Object.hasOwn(input.operations, worktree.path) });
    // A parked operation belongs to the worktree, not to the row kind. Task
    // mode used to skip this assignment, so a DevCouncil repository mid-rebase
    // rendered as idle — the exact screen the banner exists to prevent.
    if (parked && !row.operation) row.operation = parked;
    if (!byTask) {
      // The branch is the row's name; a detached worktree keeps its directory
      // name rather than rendering as an untitled row.
      row.title = branchOf(worktree) || worktree.name;
    }
    const branch = branchOf(worktree);
    if (branch.length === 0) continue;
    const set = keysByBranch.get(branch) ?? new Set<string>();
    set.add(key);
    keysByBranch.set(branch, set);
  }

  const rowKind: WorkRowKind = byTask ? "task" : "worktree";

  // Bound the cross product as well as each source: many worktrees may share
  // one branch. Report both counts whenever not every association fits.
  const MAX_ROW_LINKS = 20_000;
  let possibleLinks = 0;
  let shownLinks = 0;
  function distribute<T>(items: readonly T[], branch: (item: T) => string, bucket: (row: WorkRow) => T[]): void {
    for (const item of items) {
      const owners = keysByBranch.get(branch(item));
      possibleLinks += owners?.size ?? 1;
      if (shownLinks >= MAX_ROW_LINKS) continue;
      if (!owners) { bucket(unbound()).push(item); shownLinks += 1; continue; }
      for (const key of owners) {
        if (shownLinks >= MAX_ROW_LINKS) break;
        bucket(rowFor(key, rowKind)).push(item);
        shownLinks += 1;
      }
    }
  }
  distribute(uniqueBy(input.pullRequests ?? [], pr => pr.number), pr => pr.head_ref, row => row.pullRequests);
  distribute(uniqueBy(input.runs ?? [], run => run.id), run => run.head_branch, row => row.runs);
  const sources = { ...input.sources };
  if (shownLinks < possibleLinks) sources.github = {
    ...sources.github, ok: false, present: true,
    detail: [sources.github.detail, `${shownLinks} of ${possibleLinks} GitHub row associations shown (display limit)`].filter(Boolean).join("; "),
  };

  for (const grant of input.grants ?? []) {
    // A grant is scoped to a task and to nothing else, so in worktree mode it
    // has no row of its own to sit on and stays in the catch-all rather than
    // being attributed to a worktree that never claimed it.
    const key = byTask ? grant.scope.task_id || UNBOUND_ROW_ID : UNBOUND_ROW_ID;
    rowFor(key, "task").grants.push(grant);
  }

  for (const event of input.events ?? []) {
    // The ledger records both, so each mode joins on the column it actually
    // keys by rather than inferring one from the other.
    const key = byTask ? event.task_id || UNBOUND_ROW_ID : event.worktree_path || UNBOUND_ROW_ID;
    // An event naming a worktree git no longer lists would otherwise conjure
    // a row for a directory that is gone; those fall into the catch-all.
    const target = !byTask && key !== UNBOUND_ROW_ID && !rows.has(key) ? UNBOUND_ROW_ID : key;
    const row = rowFor(target, rowKind);
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
    sources,
    degraded: Object.values(sources).some((s: WorkSourceState) => !s.ok),
  };
}

/** How much is going on, for ordering. Never a claim about importance. */
function weight(row: WorkRow): number {
  return (
    (row.lease ? 8 : 0) +
    row.pullRequests.length * 4 +
    row.worktrees.length * 2 +
    row.runs.length +
    row.grants.length +
    (dirtyCount(row) > 0 ? 1 : 0)
  );
}

/** Preserve identity once when a source repeats records. */
function uniqueBy<T>(items: readonly T[], key: (item: T) => string | number): T[] {
  const seen = new Set<string | number>();
  return items.filter(item => {
    const id = key(item);
    if (seen.has(id)) return false;
    seen.add(id);
    return true;
  });
}

export function measuredDirty(value: number | null | undefined): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Known files and scan coverage travel together, including mixed task rows. */
export function dirtySummary(row: WorkRow): { files: number; scanned: number; total: number } {
  let files = 0;
  let scanned = 0;
  let total = 0;
  for (const { worktree } of row.worktrees) {
    if (worktree.is_bare) continue;
    total += 1;
    if (!measuredDirty(worktree.dirty_files)) continue;
    if (!Number.isSafeInteger(files + worktree.dirty_files)) continue;
    files += worktree.dirty_files;
    scanned += 1;
  }
  return { files, scanned, total };
}

/** Known uncommitted files; zero only when every worktree was measured clean. */
export function dirtyCount(row: WorkRow): number {
  const { files, scanned, total } = dirtySummary(row);
  return files > 0 || (scanned > 0 && scanned === total) ? files : -1;
}

/** Latest observed run per workflow and branch; old failures do not imply current failure. */
export function latestRuns(row: WorkRow): WorkflowRunInfo[] {
  const latest = new Map<string, WorkflowRunInfo>();
  for (const run of row.runs) {
    const key = JSON.stringify([run.head_branch, run.name]);
    const previous = latest.get(key);
    const time = Date.parse(run.created_at);
    const previousTime = previous ? Date.parse(previous.created_at) : NaN;
    if (!previous || (Number.isFinite(time) && Number.isFinite(previousTime)
      ? time > previousTime || (time === previousTime && run.id > previous.id)
      : run.id > previous.id)) latest.set(key, run);
  }
  return [...latest.values()];
}

export function rowNeedsAttention(row: WorkRow): boolean {
  const dirty = dirtySummary(row);
  return row.operation !== null || dirty.files > 0 || dirty.scanned < dirty.total ||
    row.worktrees.some(binding => binding.operationChecked === false) ||
    row.pullRequests.some(pr => pr.ci_status.toLowerCase() === "failure" || pr.review_decision.toUpperCase() === "CHANGES_REQUESTED") ||
    latestRuns(row).some(run => run.status.toLowerCase() === "completed" &&
      ["failure", "timed_out", "action_required", "startup_failure"].includes(run.conclusion.toLowerCase()));
}

/** The worktree on this row that is blocked, if any. */
export function parkedWorktree(row: WorkRow): WorktreeBinding | null {
  return row.worktrees.find((binding) => binding.operation) ?? null;
}

/**
 * The path opening this row should land on.
 *
 * A parked operation is resolved in that worktree's git dir; opening the
 * first worktree on a task that has several would send the reader to a
 * checkout that is not the one that is stuck.
 */
export function openPathFor(row: WorkRow): string {
  return parkedWorktree(row)?.worktree.path ?? row.worktrees[0]?.worktree.path ?? "";
}

function compareRows(a: WorkRow, b: WorkRow): number {
  // Parked operations stay first even when the unbound bucket holds one.
  if (Boolean(a.operation) !== Boolean(b.operation)) return a.operation ? -1 : 1;
  if (a.key === b.key) return 0;
  // Other unbound records follow named work.
  if (a.key === UNBOUND_ROW_ID) return 1;
  if (b.key === UNBOUND_ROW_ID) return -1;
  const byWeight = weight(b) - weight(a);
  if (byWeight !== 0) return byWeight;
  return a.key.localeCompare(b.key);
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

/** Counts the Work view can show without another IPC round trip. */
export interface WorkInsightSummary {
  worktrees: number;
  agentSessions: number;
  agentKinds: string[];
  dirtyWorktrees: number;
  blocked: number;
  pullRequests: number;
  unscannedDirty: number;
  unscannedOperations: number;
}

/**
 * Instant strip above the rows: derived from the join, not a second fetch.
 *
 * Collision files are not here — the projection only has dirty *counts* —
 * so overlapping paths come from `cmd_collision_risk` and must not be
 * implied from these numbers.
 */
export function insightSummary(projection: WorkProjection): WorkInsightSummary {
  let worktrees = 0;
  let dirtyWorktrees = 0;
  let unscannedDirty = 0;
  let unscannedOperations = 0;
  let blocked = 0;
  const pullRequests = new Set<number>();
  const paths: string[] = [];
  for (const row of projection.rows) {
    for (const pr of row.pullRequests) pullRequests.add(pr.number);
    if (row.operation) blocked += 1;
    for (const binding of row.worktrees) {
      worktrees += 1;
      paths.push(binding.worktree.path);
      if (binding.operationChecked === false) unscannedOperations += 1;
      if (binding.worktree.is_bare) continue;
      const dirty = binding.worktree.dirty_files;
      if (!measuredDirty(dirty)) unscannedDirty += 1;
      else if (dirty > 0) dirtyWorktrees += 1;
    }
  }
  return {
    worktrees,
    agentSessions: paths.filter((path) => isAgentWorktree(path)).length,
    agentKinds: agentKindsOn(paths),
    dirtyWorktrees,
    blocked,
    pullRequests: pullRequests.size,
    unscannedDirty,
    unscannedOperations,
  };
}

/** One sentence naming what could not be read, or empty when all of it could. */
export function degradedSummary(sources: WorkSources): string {
  const failed = Object.entries(sources)
    .filter(([, state]) => !(state as WorkSourceState).ok)
    .map(([name, state]) => `${name} (${(state as WorkSourceState).detail || "no reason given"})`);
  if (failed.length === 0) return "";
  return `This screen is incomplete: ${failed.join("; ")}.`;
}
