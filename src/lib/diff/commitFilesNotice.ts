import type { CommitDetails } from "../stores/graphStore";

/** What the commit pane says about how much of the commit it is showing. */
export interface CommitFilesSummary {
  /** The header count: the true total, the listed count, or "N+" for a floor. */
  count: string;
  /**
   * True when the churn totals and the file count come from a prefix of git's
   * output, so they are lower bounds and must not read as the commit's size.
   */
  partial: boolean;
  /** The line under the header, or null when the list is the whole commit. */
  note: string | null;
}

type Counted = Pick<
  CommitDetails,
  "changed_files" | "files_total_count" | "files_list_truncated" | "numstat_truncation_reason"
>;

/**
 * Summarises how complete a commit's file list is.
 *
 * Two different cuts reach the pane and they must not be merged: the list cap
 * (the backend knows the true count and sends it) and a cut in git's own
 * output (the true count is unknown, so every figure derived from the records
 * is a floor). The second is never rendered as an exact number.
 */
export function summarizeCommitFiles(details: Counted | null): CommitFilesSummary {
  const listed = details?.changed_files.length ?? 0;
  const reason = details?.numstat_truncation_reason;
  if (reason) {
    const sentence = reason.charAt(0).toUpperCase() + reason.slice(1);
    return {
      count: `${listed.toLocaleString()}+`,
      partial: true,
      note: `${sentence}. Only the first ${listed.toLocaleString()} files are listed and the +/- totals are partial.`,
    };
  }
  if (details?.files_list_truncated === true) {
    const total =
      typeof details.files_total_count === "number" ? details.files_total_count : listed;
    return {
      count: total.toLocaleString(),
      partial: false,
      note: `Only the first ${listed.toLocaleString()} of ${total.toLocaleString()} files are listed.`,
    };
  }
  return { count: String(listed), partial: false, note: null };
}
