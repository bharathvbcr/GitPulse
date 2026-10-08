import type { ContentMatch, ContentSearchReport, SearchTruncation } from "./types";
import { plural, shortHash } from "../format";

/** Matches grouped by file, in the order git reported the files. */
export function groupByFile(matches: readonly ContentMatch[]): { path: string; matches: ContentMatch[] }[] {
  const groups = new Map<string, ContentMatch[]>();
  for (const match of matches) {
    const group = groups.get(match.path);
    if (group) group.push(match);
    else groups.set(match.path, [match]);
  }
  return [...groups].map(([path, grouped]) => ({ path, matches: grouped }));
}

const TRUNCATION_REASON = {
  match_limit: "the match limit was reached",
  output_cap: "the output budget was reached",
  deadline: "the search hit its time limit",
  cancelled: "the search was cancelled",
} as const;

/** Narrows the wire's free-form reason to one this build can name. */
function isTruncation(value: string | null): value is SearchTruncation {
  return value !== null && Object.hasOwn(TRUNCATION_REASON, value);
}

/**
 * The one line that states what a result covers. A partial answer names the
 * reason and says plainly that more may exist, so it never reads as complete.
 */
export function describeSearchReport(report: ContentSearchReport): string {
  const where = report.revision ? `at ${shortHash(report.revision)}` : "in the working tree";
  const counted = `${plural(report.matches.length, "match", "matches")} in ${plural(report.files, "file")} ${where}`;
  if (!report.truncated) return counted;
  const reason = isTruncation(report.truncated_reason)
    ? TRUNCATION_REASON[report.truncated_reason]
    : "the search stopped early";
  return `${counted} — partial: ${reason}; more matches may exist.`;
}
