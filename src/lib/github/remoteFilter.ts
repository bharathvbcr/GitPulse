/**
 * Narrowing the Remote page's lists.
 *
 * The page shows five listings at once and offered no way to reach into any of
 * them: a repository with thirty open pull requests rendered thirty cards and
 * the reader scrolled. The counts that mattered — how many are waiting on a
 * review, how many are red — were computed for the velocity strip and then
 * not connected to anything.
 *
 * Pure functions over the payload, so the facets and the counts beside them
 * can never disagree: both call the same predicate.
 */

import type { IssueInfo } from "../ops/model";
import type { PullRequestInfo, WorkflowRunInfo } from "./types";
import { isAwaitingFirstReview } from "./prVelocity";

/** Which subset of the open pull requests is on screen. */
export type PrFacet = "all" | "awaitingReview" | "failing" | "drafts";

export const PR_FACETS: readonly PrFacet[] = [
  "all",
  "awaitingReview",
  "failing",
  "drafts",
] as const;

export const PR_FACET_LABELS: Readonly<Record<PrFacet, string>> = {
  all: "All",
  awaitingReview: "Awaiting review",
  failing: "Failing",
  drafts: "Drafts",
};

/**
 * True when a pull request's checks came back red.
 *
 * `ci_status` also carries the in-flight and the never-ran states, and neither
 * is a failure: colouring "pending" as failing would send a reader to fix a
 * pipeline that has not finished, and folding an empty status into "passing"
 * would hide a repository whose checks never start.
 */
export function prIsFailing(pr: Pick<PullRequestInfo, "ci_status">): boolean {
  const status = pr.ci_status.trim().toLowerCase();
  return status === "failure" || status === "failing" || status === "error" || status === "timed_out";
}

/** True when `pr` belongs to `facet`. */
export function prInFacet(pr: PullRequestInfo, facet: PrFacet): boolean {
  switch (facet) {
    case "all":
      return true;
    case "awaitingReview":
      return isAwaitingFirstReview(pr);
    case "failing":
      return prIsFailing(pr);
    case "drafts":
      return pr.is_draft;
  }
}

/** How many open pull requests each facet would show. */
export function prFacetCounts(
  pullRequests: readonly PullRequestInfo[],
): Record<PrFacet, number> {
  const counts = { all: 0, awaitingReview: 0, failing: 0, drafts: 0 } as Record<PrFacet, number>;
  for (const facet of PR_FACETS) {
    counts[facet] = pullRequests.filter((pr) => prInFacet(pr, facet)).length;
  }
  return counts;
}

/** Text a pull request query is matched against: number, title and both refs. */
export function prSearchText(pr: PullRequestInfo): string {
  return `#${pr.number} ${pr.title} ${pr.head_ref} ${pr.base_ref}`.toLowerCase();
}

/** Open pull requests narrowed by facet and query, in payload order. */
export function filterPullRequests(
  pullRequests: readonly PullRequestInfo[],
  facet: PrFacet,
  query: string,
): PullRequestInfo[] {
  const needle = query.trim().toLowerCase();
  return pullRequests.filter(
    (pr) => prInFacet(pr, facet) && (needle === "" || prSearchText(pr).includes(needle)),
  );
}

/** Text an issue query is matched against: number, title, author and labels. */
export function issueSearchText(issue: IssueInfo): string {
  return `#${issue.number} ${issue.title} ${issue.author} ${issue.labels.join(" ")}`.toLowerCase();
}

/** Open issues narrowed by query, in payload order. */
export function filterIssues(issues: readonly IssueInfo[], query: string): IssueInfo[] {
  const needle = query.trim().toLowerCase();
  if (needle === "") return [...issues];
  return issues.filter((issue) => issueSearchText(issue).includes(needle));
}

/**
 * Workflow runs for one branch.
 *
 * An empty `branch` means no narrowing at all, not "runs with no branch": a
 * repository whose runs all carry a head branch would otherwise go blank the
 * moment HEAD is detached.
 */
export function runsOnBranch(
  runs: readonly WorkflowRunInfo[],
  branch: string,
): WorkflowRunInfo[] {
  if (branch.trim() === "") return [...runs];
  return runs.filter((run) => run.head_branch === branch);
}

/**
 * "3m ago" for an RFC 3339 timestamp, or an empty string when there is none.
 *
 * Empty rather than "unknown time" so a caller renders nothing at all: a run
 * whose timestamp gh did not supply must not grow a label claiming it is from
 * the epoch.
 */
export function relativeAge(iso: string | null | undefined, nowMs: number): string {
  if (typeof iso !== "string" || iso.trim() === "") return "";
  const at = Date.parse(iso);
  if (!Number.isFinite(at)) return "";
  const seconds = Math.max(0, (nowMs - at) / 1000);
  if (seconds < 60) return "just now";
  const minutes = seconds / 60;
  if (minutes < 60) return `${Math.floor(minutes)}m ago`;
  const hours = minutes / 60;
  if (hours < 24) return `${Math.floor(hours)}h ago`;
  const days = hours / 24;
  if (days < 30) return `${Math.floor(days)}d ago`;
  const months = days / 30;
  if (months < 12) return `${Math.floor(months)}mo ago`;
  return `${Math.floor(days / 365)}y ago`;
}
