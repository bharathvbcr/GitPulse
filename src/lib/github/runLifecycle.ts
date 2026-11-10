/**
 * GitHub Actions' runs, reduced to a phase this app can act on.
 *
 * Whether a run is moving, delivered, broke or is waiting for a person is
 * decided once, in Rust (`src-tauri/src/github/outcome.rs`), and arrives on
 * every run as `outcome`. This module maps that to the one vocabulary the
 * delivery views share (`MonitorPhase`) and keeps no table of `status` and
 * `conclusion` words for the purpose. It had one, the work view had another,
 * and they disagreed about `action_required`; the native CI watcher needs the
 * same answer with no renderer running.
 *
 * `status` and `conclusion` stay on the run for the words a reader sees, and
 * the label tables below are only that: a run whose words this build has not
 * heard of shows them in parentheses rather than inheriting a colour.
 */
import type { MonitorPhase } from "../delivery/phase";
import { displayText, type TimelineRow } from "../delivery/timeline";
import { asRunOutcome, type RunOutcome, type WorkflowRunInfo } from "./types";

/**
 * The phase each outcome stands for.
 *
 * `cancelled` is `settled_bad` here although it is not `failed` and raises no
 * red banner: the delivery views feed one pass rate with Firebase App Hosting
 * (`rolloutState.ts`), where a cancelled rollout is a bad end, and a word that
 * meant "bad" in one panel and "no verdict" in the other would make the rate
 * unreadable. That is a statement about a rate, not about interrupting anyone.
 *
 * `needs_person` is `unknown`: settled, so the poll stops waiting on it, but it
 * is a run held for an approval, which is neither a pass nor a breakage and
 * must not enter a rate.
 */
const PHASE_OF_OUTCOME: Readonly<Record<RunOutcome, MonitorPhase>> = {
  in_flight: "in_flight",
  passed: "settled_ok",
  failed: "settled_bad",
  cancelled: "settled_bad",
  needs_person: "unknown",
  unknown: "unknown",
};

/**
 * The phase for a run. An outcome that is missing or that this build does not
 * name is `unknown`: a payload that predates the field, or a newer one, never
 * becomes a success by omission.
 */
export function runPhase(run: Pick<WorkflowRunInfo, "outcome">): MonitorPhase {
  return PHASE_OF_OUTCOME[asRunOutcome(run.outcome)];
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
  cancelled: "Canceled",
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
  if (Object.hasOwn(STATUS_LABELS, status)) return STATUS_LABELS[status];
  if (status === "completed") {
    if (conclusion === "") return "Completed (no conclusion reported)";
    return Object.hasOwn(CONCLUSION_LABELS, conclusion)
      ? CONCLUSION_LABELS[conclusion]
      : `Unknown (${conclusion})`;
  }
  if (status === "") return "Unknown (no status reported)";
  return `Unknown (${status})`;
}

/**
 * Colour for a run's state, as a status role on every verdict.
 *
 * `text-ok`, `text-danger` and `text-warn` are defined per theme, so the label
 * keeps its contrast against the light theme's near-white card as well as the
 * dark one — on exactly the labels a reader opened the panel to check.
 */
export function runStateClass(run: Pick<WorkflowRunInfo, "outcome">): string {
  const phase = runPhase(run);
  if (phase === "settled_ok") return "text-ok";
  if (phase === "settled_bad") return "text-danger";
  if (phase === "in_flight") return "text-warn";
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
