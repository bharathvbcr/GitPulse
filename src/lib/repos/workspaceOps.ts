/**
 * Running one operation across many open repositories.
 *
 * A workspace holds up to 24 repository tabs. "Fetch everything before I start
 * work" and "is anything unsaved anywhere?" are the two questions that make a
 * multi-repo client worth using, and both are unanswerable one tab at a time.
 *
 * The whole design serves one property: **a run over N repositories must
 * report what actually happened to each of them.** Three outcomes are kept
 * strictly distinct, because collapsing them is how a fleet operation comes to
 * mean nothing:
 *
 * - **ok** — the task ran and succeeded.
 * - **failed** — the task ran and failed. The reason travels with it.
 * - **skipped** — the task never ran, and why. A repository skipped because it
 *   is parked mid-merge has NOT been fetched, and must never be counted as
 *   though it had been. This is the rule that a check which could not run must
 *   not report the same result as a check that ran and passed.
 *
 * Aggregation deliberately mirrors {@link summarizeBulkOutcome}, which already
 * enforces the same honesty for per-file batches; this is its per-repository
 * counterpart rather than a second, divergent convention.
 */

import { mapWithConcurrency } from "../async/pool";

/** One repository a run will visit. */
export interface RepoTarget {
  /** Absolute repository path — the identity the backend takes. */
  path: string;
  /** Disambiguated label for display; falls back to the path. */
  label: string;
}

export type RepoTaskStatus = "ok" | "failed" | "skipped";

export interface RepoTaskResult {
  path: string;
  label: string;
  status: RepoTaskStatus;
  /** Why it failed. Present only for `failed`. */
  error?: string;
  /** Why it never ran. Present only for `skipped`. */
  reason?: string;
  /** Wall-clock milliseconds the task took. Zero for skipped repositories. */
  durationMs: number;
}

export interface BulkRunReport {
  /** Results in the order the targets were supplied — never completion order,
   *  so a report is stable and diffable between runs. */
  results: RepoTaskResult[];
  succeeded: number;
  failed: number;
  skipped: number;
  /** True when the run stopped early because it was cancelled. Remaining
   *  repositories appear as skipped with a cancellation reason. */
  cancelled: boolean;
  totalMs: number;
}

/**
 * The task a run performs against one repository.
 *
 * Returning `{ skip: reason }` declines the repository without running: the
 * task itself decides what disqualifies a repo (dirty tree, parked operation,
 * no remote), because only it knows what it is about to do.
 */
export type RepoTask = (
  target: RepoTarget,
) => Promise<void | { skip: string }>;

export interface RunOptions {
  /**
   * How many repositories are worked on at once.
   *
   * Defaults to 4. The operations that matter here are network-bound (`fetch`,
   * `pull`), and each one spawns a git subprocess; letting all 24 tabs run at
   * once saturates the connection, and every repository finishes late instead
   * of most finishing early.
   */
  concurrency?: number;
  /** Cooperative cancellation. Checked before each repository starts. */
  signal?: { aborted: boolean };
  /** Called after each repository settles, for progress rendering. */
  onProgress?: (done: number, total: number, latest: RepoTaskResult) => void;
  /** Injectable clock; the default is monotonic where available. */
  now?: () => number;
}

/** Upper bound on how many repositories one call may visit. */
export const MAX_BULK_TARGETS = 64;

/** Default parallelism — see `RunOptions.concurrency`. */
export const DEFAULT_BULK_CONCURRENCY = 4;

function defaultNow(): number {
  return typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();
}

function messageOf(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err) ?? "unknown error";
  } catch {
    return "unknown error";
  }
}

/**
 * Runs `task` against every target, at most `concurrency` at a time.
 *
 * Never rejects. A task that throws marks its own repository failed and the
 * run continues — one unreachable remote must not abandon the other 23
 * repositories, which is exactly what a plain `Promise.all` would do.
 */
export async function runAcrossRepos(
  targets: readonly RepoTarget[],
  task: RepoTask,
  options: RunOptions = {},
): Promise<BulkRunReport> {
  const now = options.now ?? defaultNow;
  const startedAt = now();
  const signal = options.signal;

  const deduped = dedupeTargets(targets).slice(0, MAX_BULK_TARGETS);
  const results: RepoTaskResult[] = new Array(deduped.length);
  // A non-finite value must fall back rather than propagate: `Math.min(NaN, n)`
  // is NaN, and `Array.from({ length: NaN })` is empty — so a stray NaN would
  // spawn zero workers and report a run in which nothing happened as though
  // every repository had been visited.
  const requested = options.concurrency;
  const bounded = Number.isFinite(requested) ? Math.floor(requested as number) : DEFAULT_BULK_CONCURRENCY;
  const concurrency = Math.max(1, Math.min(bounded, deduped.length || 1));

  let done = 0;
  let cancelled = false;

  // One canonical bounded-fan-out loop, shared with every other IPC fan-out
  // in the app; this function keeps only the per-repository reporting.
  await mapWithConcurrency(deduped.length, concurrency, async (index) => {
    const target = deduped[index];

    // Cancellation is checked per repository rather than mid-task: a
    // half-finished fetch cannot be un-run, and reporting one as "skipped"
    // would be a lie about the repository's state.
    if (signal?.aborted) {
      cancelled = true;
      results[index] = {
        path: target.path,
        label: target.label,
        status: "skipped",
        reason: "Cancelled before this repository was reached.",
        durationMs: 0,
      };
    } else {
      const taskStart = now();
      try {
        const outcome = await task(target);
        const durationMs = Math.max(0, now() - taskStart);
        results[index] =
          outcome && typeof outcome === "object" && "skip" in outcome
            ? {
                path: target.path,
                label: target.label,
                status: "skipped",
                reason: outcome.skip,
                durationMs: 0,
              }
            : {
                path: target.path,
                label: target.label,
                status: "ok",
                durationMs,
              };
      } catch (err: unknown) {
        results[index] = {
          path: target.path,
          label: target.label,
          status: "failed",
          error: messageOf(err),
          durationMs: Math.max(0, now() - taskStart),
        };
      }
    }

    done += 1;
    options.onProgress?.(done, deduped.length, results[index]);
  });

  const settled = results.filter(Boolean);
  return {
    results: settled,
    succeeded: settled.filter((r) => r.status === "ok").length,
    failed: settled.filter((r) => r.status === "failed").length,
    skipped: settled.filter((r) => r.status === "skipped").length,
    cancelled,
    totalMs: Math.max(0, now() - startedAt),
  };
}

/**
 * Drops repeated paths, keeping the first occurrence.
 *
 * Two tabs can name the same repository through different symlinks or letter
 * cases; fetching it twice concurrently makes the two runs contend for the
 * same `.git` lock and one of them fails for no real reason.
 */
function dedupeTargets(targets: readonly RepoTarget[]): RepoTarget[] {
  const seen = new Set<string>();
  const out: RepoTarget[] = [];
  for (const target of targets) {
    if (!target.path) continue;
    if (seen.has(target.path)) continue;
    seen.add(target.path);
    out.push(target);
  }
  return out;
}

/**
 * One honest sentence for a finished run.
 *
 * Skipped repositories are always named in the count when there are any:
 * "Fetched 20 of 24" reads as success with rounding, while "Fetched 20 of 24 —
 * 1 failed, 3 skipped" cannot be misread. A silent cap or omission is what
 * turns a partial sweep into a claim of full coverage.
 */
export function summarizeRun(report: BulkRunReport, verb: string): string {
  const total = report.results.length;
  if (total === 0) return `Nothing to ${verb}.`;

  const parts: string[] = [];
  if (report.failed > 0) parts.push(`${report.failed} failed`);
  if (report.skipped > 0) parts.push(`${report.skipped} skipped`);

  const head = `${verb} ${report.succeeded} of ${total}`;
  const detail = parts.length > 0 ? ` — ${parts.join(", ")}` : "";
  const cancelled = report.cancelled ? " (cancelled)" : "";
  return `${head}${detail}${cancelled}`;
}

/**
 * The first failure's reason, for a toast that needs one concrete cause.
 *
 * A count alone tells a user something went wrong but not what; the first
 * reason is almost always representative, and the rest are in the report.
 */
export function firstFailure(report: BulkRunReport): RepoTaskResult | null {
  return report.results.find((r) => r.status === "failed") ?? null;
}

/** True when every repository that ran succeeded and none were skipped. */
export function isCleanSweep(report: BulkRunReport): boolean {
  return report.failed === 0 && report.skipped === 0 && report.results.length > 0;
}
