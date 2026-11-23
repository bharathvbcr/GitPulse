/**
 * Mavi threads the quit question must name.
 *
 * A Mavi agent always ends with the app (`mavi::stop_all`), even when the
 * terminal host keeps terminal agents running, so these are listed whatever
 * the terminal backend is. Only threads in the middle of a turn are at risk:
 * an idle thread loses nothing but its process, and resumes from its
 * conversation.
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
): MaviAtRisk[] {
  return records
    .filter((record) => {
      const phase = summaryOf(record)?.phase;
      return phase === "working" || phase === "approval";
    })
    .map((record) => ({
      label: `Mavi: ${record.title}`,
      repoPath: record.target.kind === "local" ? record.target.root : `${record.target.host}:${record.target.dir}`,
      resumable: !!record.conversationId,
    }));
}
