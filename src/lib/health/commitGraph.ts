/**
 * The Health hint for a missing or stale commit-graph.
 *
 * The Rust detector (`storage/commit_graph.rs`) reads the graph files and says
 * what state the repository is in. This turns that state into the one thing a
 * person can act on. It is display only: GitPulse does not run the command,
 * schedule it, or offer a button for it. Writing or refreshing a commit-graph
 * from the app is an undecided owner decision, so the command is shown for the
 * person to run in their own terminal.
 */
import type { CommitGraphReport, CommitGraphState } from "../storage/types";

/** The command that builds a graph with changed-path Bloom filters. */
export const COMMIT_GRAPH_COMMAND = "git commit-graph write --reachable --changed-paths";

export type CommitGraphHintTone = "advice" | "unchecked";

export interface CommitGraphHint {
  /** `advice`: running the command would help. `unchecked`: nothing to run. */
  tone: CommitGraphHintTone;
  title: string;
  detail: string;
  /** Present only for `advice`. Never set when the state could not be checked. */
  command?: string;
}

const ADVICE: Partial<Record<CommitGraphState, { title: string; detail: string }>> = {
  missing: {
    title: "No commit-graph",
    detail:
      "Neither .git/objects/info/commit-graph nor a commit-graphs chain exists, so history, " +
      "ahead/behind and path-limited log walk every commit object.",
  },
  stale: {
    title: "Commit-graph is out of date",
    detail:
      "HEAD is newer than the commit-graph, so the newest history is walked without it.",
  },
  no_bloom: {
    title: "Commit-graph has no changed-path Bloom filters",
    detail:
      "The graph covers HEAD, but file history and log by path still read trees to find " +
      "the commits that touched a path.",
  },
};

/**
 * The hint for a report, or `null` when there is nothing to say: the graph is
 * present and covering HEAD, the repository has no commits, or the report
 * predates the detector (a missing field is "not checked", never "missing").
 */
export function commitGraphHint(report: CommitGraphReport | null | undefined): CommitGraphHint | null {
  if (!report) return null;
  const advice = ADVICE[report.state];
  if (advice) {
    return { tone: "advice", ...advice, command: COMMIT_GRAPH_COMMAND };
  }
  if (report.state === "unknown") {
    return {
      tone: "unchecked",
      title: "Commit-graph could not be checked",
      detail: report.note?.trim() || "GitPulse could not read the commit-graph.",
    };
  }
  return null;
}
