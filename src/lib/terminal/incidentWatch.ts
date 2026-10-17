import { toastStore } from "../stores/toastStore";
import { formatError } from "../ui/formatError";
import { hostedClaude, resumers } from "./hostedClaude";
import { INCIDENT_EVENT, createIncidentController, type ResumeReport } from "./incident";
import type { EventListen } from "./ptyBus";
import { checkTranscripts } from "./transcripts";

/**
 * What taking an incident's offer did, in one line, or null when every
 * conversation offered was relaunched. Only a process that started counts as
 * resumed; the rest are named by what happened to them.
 */
export function describeResumeReport(report: ResumeReport): string | null {
  if (report.started === report.offered) return null;
  const parts = [
    report.deferred > 0 ? `${report.deferred} waiting for a free terminal slot (each starts when one frees)` : null,
    report.failed > 0 ? `${report.failed} could not start (each tab says why)` : null,
    report.missing > 0 ? `${report.missing} no longer had a tab to resume in` : null,
  ].filter((part): part is string => part !== null);
  return `Resumed ${report.started} of ${report.offered} Claude conversations${parts.length ? `: ${parts.join("; ")}` : ""}.`;
}

/**
 * Subscribes to `terminal-incident` and puts each incident in front of the
 * person: what ended the sessions, who sent it when that is known, and one
 * "Resume N" for the Claude conversations Claude Code saved.
 *
 * The toast waits to be dismissed (duration 0): this is the one chance to
 * offer the resume, and a countdown would take the offer away with it.
 * Returns the unsubscribe.
 */
export function watchIncidents(listen: EventListen): Promise<() => void> {
  const controller = createIncidentController({
    store: hostedClaude,
    resumers,
    check: (sessionIds) => checkTranscripts(sessionIds),
    notify: ({ message, action }) => {
      toastStore.warning(message, action ?? undefined, 0);
    },
    fail: (_terminalId, cause) => {
      toastStore.error(`A Claude conversation could not be resumed: ${formatError(cause)}`);
    },
    done: (report) => {
      const line = describeResumeReport(report);
      if (line) toastStore.info(line);
    },
  });
  return listen<unknown>(INCIDENT_EVENT, (event) => {
    void controller.handle(event.payload);
  });
}
