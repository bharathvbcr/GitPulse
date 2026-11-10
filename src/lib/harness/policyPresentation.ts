/**
 * Presentation helpers for the Manvi policy cards.
 *
 * They lived in ManviHarnessPane's module script while one pane rendered
 * every card. The cards now live on different surfaces (Settings → Agents
 * for the connection, Work → Overview for the policy journal), so the
 * helpers they share live here, once, rather than in either component.
 */
import { harnessPermissionMode, type HarnessPermissionMode } from "./availability";
import type { HarnessStatus } from "../stores/harnessStore";
import type { CatchUp } from "../ingest/types";
import type { LedgerStatus } from "../ledger/types";
import { redactDiagnosticText } from "../diagnostics/diagnostics";

export type StatusTone = "ready" | "warning" | "error" | "neutral";

export interface StatusPresentation {
  label: string;
  detail: string;
  tone: StatusTone;
  cardClass: string;
  badgeClass: string;
}

export interface RefreshTimerScheduler {
  setInterval(callback: () => void, delayMs: number): unknown;
  clearInterval(handle: unknown): void;
}

export interface RepositoryRefreshTimer {
  update(repo: string | null): void;
  dispose(): void;
}

/**
 * Keeps one interval for the active repository. Updating a store with the
 * same repository identity is deliberately a no-op, so unrelated status
 * publications cannot postpone expiry/ledger refresh indefinitely.
 */
export function createRepositoryRefreshTimer(
  intervalMs: number,
  refresh: (repo: string) => void,
  scheduler: RefreshTimerScheduler,
): RepositoryRefreshTimer {
  let activeRepo: string | null = null;
  let handle: unknown;

  function clear() {
    if (handle === undefined) return;
    scheduler.clearInterval(handle);
    handle = undefined;
  }

  return {
    update(repo) {
      if (repo === activeRepo) return;
      clear();
      activeRepo = repo;
      if (!repo) return;
      const scheduledRepo = repo;
      handle = scheduler.setInterval(() => {
        if (activeRepo === scheduledRepo) refresh(scheduledRepo);
      }, intervalMs);
    },
    dispose() {
      clear();
      activeRepo = null;
    },
  };
}

const STATUS_CLASSES: Record<
  StatusTone,
  Pick<StatusPresentation, "cardClass" | "badgeClass">
> = {
  ready: {
    cardClass: "border-ok/35 bg-ok/5",
    badgeClass: "text-ok",
  },
  warning: {
    cardClass: "border-warn/35 bg-warn/5",
    badgeClass: "text-warn",
  },
  error: {
    cardClass: "border-danger/35 bg-danger/5",
    badgeClass: "text-danger",
  },
  neutral: {
    cardClass: "border-edge bg-background",
    badgeClass: "text-textMuted",
  },
};

/** The `gp-notice` tone for each status tone. "Neutral" here means nothing was determined, which is the unknown tone. */
export const NOTICE_TONE: Record<StatusTone, "ok" | "warn" | "danger" | "unknown"> = {
  ready: "ok",
  warning: "warn",
  error: "danger",
  neutral: "unknown",
};

function presentation(
  label: string,
  detail: string,
  tone: StatusTone,
): StatusPresentation {
  return { label, detail, tone, ...STATUS_CLASSES[tone] };
}

/** Truthful copy for the action runner's current policy capability. */
export function scopedRunnerPresentation(
  mode: HarnessPermissionMode,
  refreshError: string | null = null,
): StatusPresentation {
  if (refreshError) {
    return presentation(
      "Status stale",
      `Couldn't check Manvi status. ${redactDiagnosticText(refreshError)} No runner capability is claimed until status is checked again.`,
      "error",
    );
  }
  switch (mode) {
    case "connected":
      return presentation(
        "Guarded",
        "Health and coverage commands require a click, a purpose allowlist, direct argv execution, the Manvi policy gate, hard timeouts, bounded output, and stop-on-failure accounting.",
        "ready",
      );
    case "unguarded":
      return presentation(
        "Not checked",
        "User-started health and coverage commands still use a purpose allowlist, direct argv execution, hard timeouts, bounded output, and stop-on-failure accounting. Manvi is not installed, so no policy check runs.",
        "warning",
      );
    case "blocked":
      return presentation(
        "Blocked",
        "Guarded health and coverage commands are refused while Manvi policy checks are failing. Reconnect after the policy gate recovers.",
        "error",
      );
    default:
      return presentation(
        "Status unknown",
        "The Manvi policy capability has not been checked yet. User-started commands remain behind the purpose allowlist; reconnect to establish whether the policy gate can run.",
        "neutral",
      );
  }
}

/** The label of the action that keeps a refused ledger. */
export const ADOPT_LEDGER_LABEL = "Adopt this ledger";

/**
 * Whether the history has a problem the adoption action might resolve: the
 * ledger reports an error, or catch-up does. The panel does not read which
 * error it is (that is the backend's to say, and it does: it refuses an
 * adoption that does not apply with its own reason). Offering the action on any
 * history error costs a refused request; hiding it behind text matching costs
 * a person the only way out.
 */
export function adoptionOffered(
  ledger: LedgerStatus | null,
  catchUp: Pick<CatchUp, "error"> | null,
): boolean {
  return Boolean(ledger?.error) || Boolean(catchUp?.error);
}

/**
 * Separately reports UI refresh, durable recording, and external catch-up.
 * One successful source must never hide a gap in another source.
 */
export function activityHistoryPresentations(
  refreshError: string | null,
  ledger: LedgerStatus | null,
  catchUp: CatchUp | null,
): StatusPresentation[] {
  const statuses: StatusPresentation[] = [];

  if (refreshError) {
    statuses.push(
      presentation(
        "Couldn't check Manvi status",
        `${redactDiagnosticText(refreshError)} Displayed capability status may be stale.`,
        "error",
      ),
    );
  }

  if (!ledger) {
    statuses.push(
      presentation(
        "Activity history not checked",
        "Durable recording status has not loaded for this repository.",
        "neutral",
      ),
    );
  } else if (!ledger.recording || ledger.error) {
    const details = [
      ledger.error
        ? redactDiagnosticText(ledger.error)
        : "Durable activity recording is unavailable.",
      ledger.dropped > 0
        ? `${ledger.dropped} ${ledger.dropped === 1 ? "event was" : "events were"} not recorded.`
        : "",
    ].filter(Boolean);
    statuses.push(presentation("Activity history incomplete", details.join(" "), "error"));
  } else if (ledger.dropped > 0) {
    statuses.push(
      presentation(
        "Activity history incomplete",
        `${ledger.dropped} ${ledger.dropped === 1 ? "event was" : "events were"} not recorded.`,
        "warning",
      ),
    );
  } else {
    statuses.push(
      presentation(
        "Activity history recording",
        ledger.path
          ? `Durable events are recording at ${ledger.path}.`
          : "Durable events are recording for this repository.",
        "ready",
      ),
    );
  }

  if (!catchUp) {
    statuses.push(
      presentation(
        "Catch-up not checked",
        "External transcript and reflog catch-up has not completed for this repository.",
        "neutral",
      ),
    );
  } else if (catchUp.error || catchUp.skipped_lines > 0) {
    const details = [
      catchUp.error ? redactDiagnosticText(catchUp.error) : "",
      catchUp.skipped_lines > 0
        ? `${catchUp.skipped_lines} transcript ${catchUp.skipped_lines === 1 ? "line was" : "lines were"} skipped.`
        : "",
      catchUp.recorded > 0
        ? `${catchUp.recorded} ${catchUp.recorded === 1 ? "event was" : "events were"} still recovered.`
        : "",
    ].filter(Boolean);
    statuses.push(presentation("Catch-up incomplete", details.join(" "), "warning"));
  } else {
    statuses.push(
      presentation(
        "Catch-up complete",
        catchUp.recorded > 0
          ? `${catchUp.recorded} external ${catchUp.recorded === 1 ? "event was" : "events were"} recovered from ${catchUp.transcripts} ${catchUp.transcripts === 1 ? "transcript" : "transcripts"} and ${catchUp.reflog_entries} reflog ${catchUp.reflog_entries === 1 ? "entry" : "entries"}.`
          : "No external transcript or reflog events needed to be recovered.",
        "ready",
      ),
    );
  }

  return statuses;
}

/**
 * Whether Work → Overview offers the policy journal at all.
 *
 * Hidden only when Manvi is positively known to be absent and the journal is
 * empty: with nothing installed there is nothing to journal. Every other
 * state keeps it, because each is a reason to read it. A status that has not
 * been probed yet, or that failed, is not "no Manvi" (hiding the card then
 * would turn a check that could not run into a clean bill), and a blocked
 * gate or any recorded row is exactly what the journal exists to show.
 */
export function policyActivityVisible(
  harness: HarnessStatus | null,
  actionCount: number,
  statusError: string | null,
): boolean {
  return (
    harnessPermissionMode(harness) !== "unguarded" || actionCount > 0 || statusError !== null
  );
}
