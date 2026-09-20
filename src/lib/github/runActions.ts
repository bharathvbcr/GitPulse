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
export function ciLocalVerdict(report: {
  passed: number;
  failed: number;
  skipped: number;
  test_scope?: { mode: string; fail_closed: boolean } | null;
}): string {
  const scopeLabel =
    report.test_scope?.mode === "affected" && !report.test_scope.fail_closed
      ? "affected tests"
      : "full suite";
  if (report.failed > 0) {
    return `${scopeLabel} failed (${report.failed} step${report.failed === 1 ? "" : "s"})`;
  }
  if (report.skipped > 0) {
    return `${scopeLabel} passed with ${report.skipped} skipped`;
  }
  return `${scopeLabel} passed (${report.passed} steps)`;
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

/* --- CI rail previews ------------------------------------------------------
   The rail is one column, and the deploy section is the last thing in it. Every
   workflow, run and release row pushes Firebase App Hosting further down, and
   on a repository with a normal amount of CI history it ended up below a fold
   nobody scrolls to — a section that loads fine, unreachable in practice.

   So the long listings render a preview and expand on request. The counts are
   per list because the rows are not the same size, but the mechanics live in
   `previewList` — a copy per section is how two lists end up disagreeing
   about what "show all" means. */

export {
  expandLabel,
  overflowsPreview,
  previewCap,
  previewSlice,
} from "../ui/previewList";

/** Workflow rows rendered before the reader asks for the rest. */
export const WORKFLOW_PREVIEW_COUNT = 5;

/**
 * Run cards rendered before the reader asks for the rest.
 *
 * The backend fetches twenty, and the duration timeline above these cards
 * previews the same three. Five used to bury the rest of the rail; three
 * leaves the actions reachable without a second scroll.
 */
export const RUN_PREVIEW_COUNT = 3;

/**
 * Rendered-row count at which rows tighten.
 *
 * Measured against the rows actually on screen, not the fetched total: a
 * collapsed preview is short enough to stay comfortable, and it is the
 * expanded list — the one the reader deliberately opened — that has to stay
 * navigable.
 */
export const COMPACT_ROW_THRESHOLD = 8;

/** Whether `shown` rows are enough to warrant the tighter row. */
export function useCompactRows(shown: number): boolean {
  return shown >= COMPACT_ROW_THRESHOLD;
}
