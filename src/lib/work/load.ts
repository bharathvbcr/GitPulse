import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../ui/formatError";
import type { GitHubContext } from "../github/types";
import type { GrantView, Grant } from "../grants/types";
import type { LedgerEvent } from "../ledger/types";
import type { TaskScope, TaskView } from "../tasks/types";
import type { WorktreeInfo } from "../branches/types";
import type { RepoOperation } from "../repos/operation";
import { projectWork, type WorkInputs, type WorkProjection, type WorkSources } from "./projection";

/**
 * Gathering the five sources the Work view joins.
 *
 * Each is fetched independently and each failure is recorded rather than
 * thrown: one source being down must degrade the screen, not blank it. What
 * the projection then renders is "here is what we could see, and here is what
 * we could not" — which is the only honest thing a joined view can say.
 */

/** Ledger rows read for the verdict tally. */
export const LEDGER_WINDOW = 500;

/** Worktrees whose task binding is resolved. Each costs one IPC round trip. */
export const MAX_BINDING_LOOKUPS = 64;

/** Task scopes fetched for their titles. */
export const MAX_TITLE_LOOKUPS = 32;

/**
 * Worktrees probed for a parked merge/rebase/cherry-pick. One IPC round trip
 * each, and the probe is a single `git rev-parse` on an idle worktree — but a
 * repository with hundreds of worktrees must not turn opening this screen into
 * hundreds of subprocesses, so the sweep is bounded like every other here.
 */
export const MAX_OPERATION_PROBES = 32;

export interface WorkLoadDeps {
  invoke?: typeof invoke;
}

const ABSENT = { ok: true, present: false, detail: "" } as const;
const READ = { ok: true, present: true, detail: "" } as const;

function failed(e: unknown): WorkSources[keyof WorkSources] {
  return { ok: false, present: true, detail: formatError(e) };
}

/**
 * Records a second failure on a source that may already be degraded.
 *
 * Overwriting the first reason with the second would hide the one the
 * reader has not yet been told about; concatenating keeps both.
 */
function noteFailure(
  source: WorkSources[keyof WorkSources],
  detail: string,
): WorkSources[keyof WorkSources] {
  if (!source.ok && source.detail) {
    return { ok: false, present: true, detail: `${source.detail}; ${detail}` };
  }
  return { ok: false, present: true, detail };
}

/**
 * Loads and joins everything the Work view shows for one repository.
 *
 * Never rejects: a failure anywhere becomes a degraded source in the result.
 * The caller renders a projection either way, because a screen that vanishes
 * on one bad source tells the reader less than one that says which source.
 */
export async function loadWork(
  repoPath: string,
  deps: WorkLoadDeps = {},
): Promise<WorkProjection> {
  const call = deps.invoke ?? invoke;

  const sources: WorkSources = {
    tasks: { ...ABSENT },
    worktrees: { ...ABSENT },
    github: { ...ABSENT },
    ledger: { ...ABSENT },
    grants: { ...ABSENT },
  };

  const input: WorkInputs = {
    leases: null,
    titles: {},
    worktrees: null,
    bindings: {},
    pullRequests: null,
    runs: null,
    events: null,
    grants: null,
    operations: {},
    sources,
  };

  // Fetched together: they do not depend on each other, and the view is not
  // worth five sequential round trips.
  const [tasks, worktrees, github, ledger, grants] = await Promise.all([
    call<TaskView>("cmd_task_view", { repoPath }).then(
      (v) => ({ ok: true as const, v }),
      (e) => ({ ok: false as const, e }),
    ),
    call<WorktreeInfo[]>("cmd_list_worktrees", { repoPath }).then(
      (v) => ({ ok: true as const, v }),
      (e) => ({ ok: false as const, e }),
    ),
    call<GitHubContext>("cmd_github_context", { repoPath }).then(
      (v) => ({ ok: true as const, v }),
      (e) => ({ ok: false as const, e }),
    ),
    call<LedgerEvent[]>("cmd_ledger_tail", { repoPath, cursor: 0, limit: LEDGER_WINDOW }).then(
      (v) => ({ ok: true as const, v }),
      (e) => ({ ok: false as const, e }),
    ),
    call<GrantView>("cmd_grants_view", { repoPath }).then(
      (v) => ({ ok: true as const, v }),
      (e) => ({ ok: false as const, e }),
    ),
  ]);

  if (!tasks.ok) {
    sources.tasks = failed(tasks.e);
  } else if (!tasks.v.available) {
    // No DevCouncil store here. Ordinary, and not a failure — but the leases
    // list stays null so the projection never reports "no tasks" for a
    // repository that has no task model at all.
    sources.tasks = { ...ABSENT };
  } else if (tasks.v.error) {
    sources.tasks = { ok: false, present: true, detail: tasks.v.error };
  } else {
    sources.tasks = { ...READ };
    input.leases = tasks.v.leases;
  }

  if (!worktrees.ok) {
    sources.worktrees = failed(worktrees.e);
  } else {
    sources.worktrees = { ...READ };
    input.worktrees = worktrees.v;

    // Parked operations, per worktree. `cmd_repo_operation` answers for the
    // worktree it is given — MERGE_HEAD and friends live in the linked
    // worktree's own git dir — so this is the only way to learn that the
    // worktree in the NEXT window is stuck mid-rebase.
    //
    // Filter before slicing: a bare entry at the front used to consume a
    // probe slot, so a later non-bare worktree past the cap was never asked
    // and never reported as unasked. A probe that throws is a degraded
    // source, not an idle worktree — showing nothing because the check
    // itself broke is the failure mode that strands a user mid-rebase with
    // a UI insisting everything is fine.
    const candidates = worktrees.v.filter((w) => !w.is_bare);
    const probes = candidates.slice(0, MAX_OPERATION_PROBES);
    if (candidates.length > probes.length) {
      sources.worktrees = noteFailure(
        sources.worktrees,
        `only the first ${MAX_OPERATION_PROBES} of ${candidates.length} worktrees were probed for parked operations`,
      );
    }
    const parked = await Promise.all(
      probes.map((w) =>
        call<RepoOperation | null>("cmd_repo_operation", { repoPath: w.path }).then(
          (v) => ({ path: w.path, operation: v, ok: true as const }),
          (e) => ({ path: w.path, operation: null, ok: false as const, detail: formatError(e) }),
        ),
      ),
    );
    const operations: Record<string, RepoOperation | null> = {};
    let probeFailures = 0;
    let probeFailureDetail = "";
    for (const result of parked) {
      if (!result.ok) {
        probeFailures += 1;
        if (!probeFailureDetail) probeFailureDetail = result.detail;
        continue;
      }
      if (result.operation) operations[result.path] = result.operation;
    }
    if (probeFailures > 0) {
      const why = probeFailureDetail ? ` (${probeFailureDetail})` : "";
      sources.worktrees = noteFailure(
        sources.worktrees,
        `could not read parked operations for ${probeFailures} worktree${probeFailures === 1 ? "" : "s"}${why}`,
      );
    }
    input.operations = operations;
  }

  if (!github.ok) {
    sources.github = failed(github.e);
  } else if (!github.v.available) {
    sources.github = { ...ABSENT };
  } else if (github.v.error) {
    sources.github = { ok: false, present: true, detail: github.v.error };
  } else {
    // A section that failed inside an otherwise usable context degrades this
    // source too: the runs list being empty because `gh` choked is not the
    // same fact as there being no runs.
    const sectionError = github.v.runs_error ?? "";
    sources.github = sectionError
      ? { ok: false, present: true, detail: sectionError }
      : { ...READ };
    input.pullRequests = github.v.pull_requests;
    input.runs = github.v.workflow_runs;
  }

  if (!ledger.ok) {
    sources.ledger = failed(ledger.e);
  } else {
    sources.ledger = { ...READ };
    input.events = ledger.v;
  }

  if (!grants.ok) {
    sources.grants = failed(grants.e);
  } else if (!grants.v.available) {
    sources.grants = { ...ABSENT };
  } else if (grants.v.error) {
    sources.grants = { ok: false, present: true, detail: grants.v.error };
  } else {
    sources.grants = { ...READ };
    input.grants = grants.v.grants as Grant[];
  }

  // Worktree → task bindings. One call each, so it is capped; a worktree past
  // the cap lands in the unbound bucket, and `worktrees` is marked degraded so
  // that never silently reads as "bound to nothing".
  if (input.worktrees && input.worktrees.length > 0) {
    const looked = input.worktrees.slice(0, MAX_BINDING_LOOKUPS);
    const bindings: Record<string, string> = {};
    await Promise.all(
      looked.map(async (worktree) => {
        try {
          const taskId = await call<string | null>("cmd_worktree_task", {
            repoPath,
            worktreePath: worktree.path,
          });
          if (taskId) bindings[worktree.path] = taskId;
        } catch {
          // One unreadable binding is not a reason to lose the other rows;
          // the worktree simply appears unbound, which the degraded flag below
          // covers when it was the cap rather than the data.
        }
      }),
    );
    input.bindings = bindings;
    if (input.worktrees.length > looked.length) {
      sources.worktrees = noteFailure(
        sources.worktrees,
        `only the first ${MAX_BINDING_LOOKUPS} of ${input.worktrees.length} worktrees ` +
          `had their task binding resolved`,
      );
    }
  }

  // Titles for the tasks that ended up on screen.
  const taskIds = [...new Set((input.leases ?? []).map((l) => l.task_id))].slice(
    0,
    MAX_TITLE_LOOKUPS,
  );
  if (taskIds.length > 0) {
    const titles: Record<string, string> = {};
    await Promise.all(
      taskIds.map(async (taskId) => {
        try {
          const scope = await call<TaskScope | null>("cmd_task_scope", { repoPath, taskId });
          if (scope?.title) titles[taskId] = scope.title;
        } catch {
          // A missing title is cosmetic: the row is identified by its id.
        }
      }),
    );
    input.titles = titles;
  }

  return projectWork(input);
}
