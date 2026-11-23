/**
 * Mavi threads the quit question must name.
 *
 * A Mavi agent always ends with the app (`mavi::stop_all`), even when the
 * terminal host keeps terminal agents running, so these are listed whatever
 * the terminal backend is. Only threads in the middle of a turn are at risk:
 * an idle thread loses nothing but its process, and resumes from its
 * conversation. Messages queued behind a running turn are named too: the
 * quit does not send them, and they come back as the thread's draft.
 */
import type { MaviSummary } from "./types";
import type { ThreadRecord } from "./threads";

export interface MaviAtRisk {
  label: string;
  repoPath: string;
  /** The conversation can be resumed in Mavi after relaunching. */
  resumable: boolean;
}

export function maviQuitRisks(
  records: readonly ThreadRecord[],
  summaryOf: (record: ThreadRecord) => Pick<MaviSummary, "phase"> | null,
  queuedOf: (record: ThreadRecord) => number = () => 0,
): MaviAtRisk[] {
  return records
    .filter((record) => {
      const phase = summaryOf(record)?.phase;
      return phase === "working" || phase === "approval";
    })
    .map((record) => {
      const queued = queuedOf(record);
      // Queued messages are not sent by a quit; they come back as the draft.
      const note =
        queued > 0
          ? ` (${queued} queued ${queued === 1 ? "message" : "messages"}, kept as a draft)`
          : "";
      return {
        label: `Mavi: ${record.title}${note}`,
        repoPath:
          record.target.kind === "local"
            ? record.target.root
            : `${record.target.host}:${record.target.dir}`,
        resumable: !!record.conversationId,
      };
    });
}
