/**
 * Cross-repo link-candidate honesty for the Map panel.
 */

import type { WorkspaceLinksResult } from "../codeintel/types";

export function linkCandidatesHonesty(result: WorkspaceLinksResult | null): string | null {
  if (!result) return null;
  if (result.repos_considered === 0) {
    return "No repos in the workspace registry yet. Open more tabs or wait for sync.";
  }
  const gaps = result.coverage_gaps.length;
  const unread = gaps === 0 ? "" : ` ${gaps} item(s) could not be read (${result.coverage_gaps.slice(0, 3).map((g) => `${g.repo}: ${g.reason}`).join("; ")}${gaps > 3 ? "; ..." : ""}), so this list may be incomplete.`;
  if (result.count === 0) {
    return `No cross-repo import candidates across ${result.repos_considered} registered repo(s).${unread}`;
  }
  if (result.links.length < result.count) {
    return `Showing ${result.links.length} of ${result.count} candidates across ${result.repos_considered} repo(s).${unread}`;
  }
  return `${result.count} candidate(s) across ${result.repos_considered} repo(s).${unread}`;
}
