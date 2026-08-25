export interface BranchCleanupCandidate {
  name: string;
  last_summary: string;
  last_author: string;
  last_commit_timestamp: number;
  upstream_gone: boolean;
}

export interface BranchCleanupPlan {
  default_branch: string;
  current_branch: string;
  total_local_branches: number;
  protected_branches: number;
  unmerged_branches: number;
  candidates: BranchCleanupCandidate[];
}

export type ReviewSeverity = "error" | "warning" | "info";

export interface CommitMessageFinding {
  commit_id: string;
  short_id: string;
  subject: string;
  severity: ReviewSeverity;
  code: string;
  detail: string;
}

export interface CommitReviewReport {
  range: string;
  total_commits: number;
  reviewed_commits: number;
  truncated: boolean;
  conventional_commits: number;
  issue_linked_commits: number;
  findings: CommitMessageFinding[];
}

export interface IssueInfo {
  number: number;
  title: string;
  state: string;
  url: string;
  labels: string[];
  updated_at: string;
  author: string;
}

export interface ReleasePublishResult {
  tag: string;
  remote: string;
  created_tag: boolean;
  output: string;
}

const STABLE_RELEASE = /^v(\d+)\.(\d+)\.(\d+)$/;

/** Suggests one patch past the highest stable vMAJOR.MINOR.PATCH tag. */
export function releaseTagSuggestion(tags: string[]): string {
  let best: [number, number, number] | null = null;
  for (const tag of tags) {
    const match = STABLE_RELEASE.exec(tag);
    if (!match) continue;
    const candidate: [number, number, number] = [
      Number(match[1]),
      Number(match[2]),
      Number(match[3]),
    ];
    if (
      !best ||
      candidate[0] > best[0] ||
      (candidate[0] === best[0] && candidate[1] > best[1]) ||
      (candidate[0] === best[0] && candidate[1] === best[1] && candidate[2] > best[2])
    ) {
      best = candidate;
    }
  }
  if (!best) return "v0.1.0";
  return `v${best[0]}.${best[1]}.${best[2] + 1}`;
}

export function summarizeCommitReview(report: CommitReviewReport): string {
  if (report.truncated) {
    return `Reviewed ${report.reviewed_commits} of ${report.total_commits} outgoing commits (capped).`;
  }
  return `Reviewed all ${report.reviewed_commits} outgoing commits.`;
}
