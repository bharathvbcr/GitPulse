/**
 * Cross-repo link-candidate honesty for the Map panel.
 */

import type { WorkspaceLinksResult } from "../codeintel/types";

export function linkCandidatesHonesty(result: WorkspaceLinksResult | null): string | null {
  if (!result) return null;
  if (result.repos_considered === 0) {
    return "No repos in the workspace registry yet. Open more tabs or wait for sync.";
  }
  if (result.count === 0) {
    return `No cross-repo import candidates across ${result.repos_considered} registered repo(s).`;
  }
  if (result.links.length < result.count) {
    return `Showing ${result.links.length} of ${result.count} candidates across ${result.repos_considered} repo(s).`;
  }
  return `${result.count} candidate(s) across ${result.repos_considered} repo(s).`;
}
