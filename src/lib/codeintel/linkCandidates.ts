/**
 * Cross-repo link-candidate honesty for the Map panel.
 */

import type { WorkspaceLinksResult } from "../codeintel/types";

export function linkCandidatesHonesty(result: WorkspaceLinksResult | null): string | null {
  if (!result) return null;
  if (result.repos_considered === 0) {
    return "No repositories in the workspace registry yet. Open more tabs or wait for sync.";
  }
  const gaps = result.coverage_gaps.length;
  const unread = gaps === 0 ? "" : ` ${gaps} item(s) couldn't be read (${result.coverage_gaps.slice(0, 3).map((g) => `${g.repo}: ${g.reason}`).join("; ")}${gaps > 3 ? "; ..." : ""}), so this list may be incomplete.`;
  if (result.count === 0) {
    return `No cross-repository import candidates across ${result.repos_considered} registered ${result.repos_considered === 1 ? "repository" : "repositories"}.${unread}`;
  }
  if (result.links.length < result.count) {
    return `Showing ${result.links.length} of ${result.count} candidates across ${result.repos_considered} ${result.repos_considered === 1 ? "repository" : "repositories"}.${unread}`;
  }
  return `${result.count} ${result.count === 1 ? "candidate" : "candidates"} across ${result.repos_considered} ${result.repos_considered === 1 ? "repository" : "repositories"}.${unread}`;
}
