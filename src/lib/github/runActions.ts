/**
 * Pure predicates and labels for the GitHub CI/CD actions in the GitHub
 * panel. Kept out of the component so the run-state vocabulary — which must
 * match both gh's statuses and the backend's gating — is unit-testable.
 */
import type { WorkflowRunInfo } from "./types";

/** A finished run can be re-run (`gh run rerun`). */
export function canRerunRun(run: Pick<WorkflowRunInfo, "status">): boolean {
  return run.status.toLowerCase() === "completed";
}

/**
 * An in-flight run can be cancelled (`gh run cancel`). Queued and waiting
 * count: both occupy the pipeline — `waiting` sits behind deployment
 * protection rules, `requested` behind an approval — and either can sit
 * there for a long time.
 */
export function canCancelRun(run: Pick<WorkflowRunInfo, "status">): boolean {
  const status = run.status.toLowerCase();
  return (
    status === "in_progress" ||
    status === "queued" ||
    status === "pending" ||
    status === "waiting" ||
    status === "requested"
  );
}

const WORKFLOW_STATE_LABELS: Record<string, string> = {
  active: "active",
  disabled_manually: "disabled",
  disabled_inactivity: "inactive",
};

/** gh's workflow state rendered for the UI; unknown states pass through. */
export function workflowStateLabel(state: string): string {
  return WORKFLOW_STATE_LABELS[state] ?? state;
}

/** Only `active` workflows accept a `workflow_dispatch` event. */
export function isWorkflowDispatchable(state: string): boolean {
  return state === "active";
}

/** One-line verdict for a local-CI report, mirroring CI badge semantics. */
export function ciLocalVerdict(report: { passed: number; failed: number; skipped: number }): string {
  if (report.failed > 0) return `failed (${report.failed} step${report.failed === 1 ? "" : "s"})`;
  if (report.skipped > 0) return `passed with ${report.skipped} skipped`;
  return `passed (${report.passed} steps)`;
}

/**
 * Tailwind class for a local-CI step status pill; unknown stays muted.
 *
 * Both shades, always. A single `-400` is tuned for the dark theme and sits
 * at roughly 2:1 against the light theme's near-white surface — legible
 * enough to look deliberate, not legible enough to read, on exactly the
 * labels a reader is here to check.
 */
export function ciStepClass(status: string): string {
  switch (status) {
    case "passed":
      return "text-green-700 dark:text-green-400";
    case "failed":
      return "text-red-700 dark:text-red-400";
    default:
      return "text-textMuted";
  }
}
