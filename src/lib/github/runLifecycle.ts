/**
 * GitHub Actions' run vocabulary, reduced to a phase this app can act on.
 *
 * The sibling of `firebase/rolloutState.ts`, deliberately shaped the same way
 * and for the same reasons: closed sets, no fallthrough to a verdict, and a
 * state this build has never heard of shows its raw string rather than
 * inheriting a colour it did not earn.
 *
 * Actions splits what Firebase keeps in one field. `status` says whether the
 * run is moving; `conclusion` says how it ended and is meaningless until it
 * has. Reading `conclusion` without checking `status` first is how a run that
 * is still executing gets reported as a success: an in-progress run's
 * conclusion is empty, and empty is not failure, so a naive
 * `conclusion !== "failure"` check calls every running job green.
 *
 * Vocabulary source: `gh run list --help` from gh 2.101.0, which documents the
 * union of both fields as its `--status` filter values —
 * `queued|completed|in_progress|requested|waiting|pending|action_required|
 * cancelled|failure|neutral|skipped|stale|startup_failure|success|timed_out`.
 * Captured from the tool rather than recalled; an invented list here would
 * silently misclassify whatever it got wrong.
 */
import type { MonitorPhase } from "../delivery/phase";
import { displayText, type TimelineRow } from "../delivery/timeline";
import type { WorkflowRunInfo } from "./types";

/**
 * Statuses that mean the run has not finished.
 *
 * `waiting` and `pending` are in here because a run held for a deployment
 * approval or a concurrency group genuinely has not settled — but note that
 * both are bounded by the poll session's own ceiling, so a run waiting for an
 * approval that never comes stops being polled rather than forever.
 */
const IN_FLIGHT_STATUS: ReadonlySet<string> = new Set([
  "queued",
  "in_progress",
  "requested",
  "waiting",
  "pending",
]);

/** The one status that means `conclusion` is now worth reading. */
const COMPLETED_STATUS = "completed";

/** Conclusions that mean the run delivered. A closed set of exactly one. */
const OK_CONCLUSION: ReadonlySet<string> = new Set(["success"]);

/**
 * Conclusions that mean the run finished and did not deliver.
 *
 * `cancelled` is counted as a failure to match `rolloutState.ts`, which made
 * the same call for App Hosting. The two sources feed one pass rate and one
 * set of notices, and a word that means "bad" in one panel and "no verdict" in
 * the other would make that rate unreadable.
 *
 * What is deliberately NOT here: `neutral`, `skipped`, `stale` and
 * `action_required`. None of them is a failure. A run awaiting manual approval
 * has not broken anything, and a notice that cries failure over one teaches
 * people to stop reading the notices.
 */
const BAD_CONCLUSION: ReadonlySet<string> = new Set([
  "failure",
  "cancelled",
  "timed_out",
  "startup_failure",
]);

/**
 * The phase for a run, from both of its fields.
 *
 * Every path that is not an explicit member of a closed set returns `unknown`,
 * including the one that matters most: `status: completed` with an empty
 * `conclusion`. That happens whenever the conclusion could not be read, and it
 * must never be a success.
 */
export function runPhase(run: Pick<WorkflowRunInfo, "status" | "conclusion">): MonitorPhase {
  const status = (run.status ?? "").trim().toLowerCase();
  const conclusion = (run.conclusion ?? "").trim().toLowerCase();
  if (IN_FLIGHT_STATUS.has(status)) return "in_flight";
  if (status !== COMPLETED_STATUS) return "unknown";
  if (OK_CONCLUSION.has(conclusion)) return "settled_ok";
  if (BAD_CONCLUSION.has(conclusion)) return "settled_bad";
  return "unknown";
}

/** Human labels for the words gh reports. */
const STATUS_LABELS: Record<string, string> = {
  queued: "Queued",
  in_progress: "Running",
  requested: "Requested",
  waiting: "Waiting",
  pending: "Pending",
};

const CONCLUSION_LABELS: Record<string, string> = {
  success: "Passed",
  failure: "Failed",
  cancelled: "Cancelled",
  timed_out: "Timed out",
  startup_failure: "Startup failed",
  neutral: "Neutral",
  skipped: "Skipped",
  stale: "Stale",
  action_required: "Action required",
};

/**
 * What the reader sees for a run's state.
 *
 * An unrecognised word is shown in parentheses rather than replaced, so
 * someone looking at an unfamiliar badge can search for the real string — the
 * same contract `rolloutStateLabel` keeps.
 */
export function runStateLabel(run: Pick<WorkflowRunInfo, "status" | "conclusion">): string {
  const status = (run.status ?? "").trim().toLowerCase();
  const conclusion = (run.conclusion ?? "").trim().toLowerCase();
  if (IN_FLIGHT_STATUS.has(status)) return STATUS_LABELS[status] ?? `Unknown (${status})`;
  if (status === COMPLETED_STATUS) {
    if (conclusion === "") return "Completed (no conclusion reported)";
    return CONCLUSION_LABELS[conclusion] ?? `Unknown (${conclusion})`;
  }
  if (status === "") return "Unknown (no status reported)";
  return `Unknown (${status})`;
}

/**
 * Colour for a run's state, with both shades on every verdict.
 *
 * A bare `-400` is tuned for the dark theme and sits near 2:1 against the
 * light theme's near-white card — on exactly the labels a reader opened the
 * panel to check. Kept identical to `rolloutStateClass` so a failed deploy and
 * a failed run are the same red.
 */
export function runStateClass(run: Pick<WorkflowRunInfo, "status" | "conclusion">): string {
  const phase = runPhase(run);
  if (phase === "settled_ok") return "text-green-700 dark:text-green-400";
  if (phase === "settled_bad") return "text-red-700 dark:text-red-400";
  if (phase === "in_flight") return "text-amber-700 dark:text-amber-400";
  return "text-textMuted";
}

/**
 * A run as a timeline row.
 *
 * `startedAt` is `started_at` and never `created_at`. They differ by however
 * long the run waited for a runner, and using the creation time as a start
 * would report queue time as execution time — a run that waited twenty minutes
 * and executed for ten would be drawn as the slowest in the sample.
 */
export function runTimelineRow(run: WorkflowRunInfo): TimelineRow {
  return {
    id: String(run.id),
    label: displayText(run.title) || displayText(run.name) || `Run ${run.id}`,
    sublabel:
      run.title && run.name && run.title !== run.name ? displayText(run.name) : "",
    phase: runPhase(run),
    stateLabel: runStateLabel(run),
    startedAt: run.started_at ?? "",
    endedAt: run.updated_at ?? "",
    commitSha: run.head_sha ?? "",
    branch: run.head_branch ?? "",
    trigger: run.event ?? "",
    url: run.url ?? "",
  };
}

/** Every run as a timeline row, in the order gh returned them (newest first). */
export function runTimelineRows(runs: readonly WorkflowRunInfo[]): TimelineRow[] {
  return runs.map(runTimelineRow);
}
