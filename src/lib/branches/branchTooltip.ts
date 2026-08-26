import type { BranchInfo, TagInfo } from "./types";
import { formatRelativeTime } from "../format";

/**
 * Rich hover text for the branch tree.
 *
 * The row's native `title` used to carry only name + last summary, leaving
 * data the sidebar already owns (author, age, ahead/behind, upstream) one
 * context menu away. The global Tooltip upgrades every title into a themed
 * multi-line pill and mirrors it into aria-label, so this single string is
 * both the sighted and the screen-reader surface.
 */

/** "↑2 ↓1" style counts; empty when both are zero. */
function aheadBehind(branch: BranchInfo): string {
  const parts: string[] = [];
  if (branch.ahead_count > 0) parts.push(`ahead ${branch.ahead_count}`);
  if (branch.behind_count > 0) parts.push(`behind ${branch.behind_count}`);
  return parts.join(", ");
}

export function branchTooltip(branch: BranchInfo, nowSec?: number): string {
  const lines: string[] = [branch.name];

  const summary = branch.last_summary?.trim();
  if (summary) lines.push(summary);

  const attribution = [
    branch.last_author?.trim() || null,
    branch.last_commit_timestamp
      ? formatRelativeTime(branch.last_commit_timestamp, nowSec) || null
      : null,
  ]
    .filter(Boolean)
    .join(" · ");
  if (attribution) lines.push(attribution);

  const upstream = branch.upstream?.trim() || (branch.is_remote ? branch.remote_name?.trim() || null : null);
  const counts: string[] = [];
  const ab = aheadBehind(branch);
  if (ab) counts.push(ab);
  if (!branch.is_remote && branch.commits_ahead_of_base > 0 && !branch.is_current) {
    counts.push(`+${branch.commits_ahead_of_base} vs base`);
  }
  if (upstream) counts.push(`tracks ${upstream}`);

  const flags: string[] = [];
  if (branch.is_default) flags.push("default");
  if (branch.is_current) flags.push("checked out");
  if (branch.is_gone) flags.push("upstream gone");

  const meta = [...counts, ...flags].join(" · ");
  if (meta) lines.push(meta);

  return lines.join("\n");
}

export function tagTooltip(tag: TagInfo): string {
  const lines = [`Tag ${tag.name}`];
  const message = tag.message?.trim();
  if (message) lines.push(message);
  if (tag.commit_id) lines.push(tag.commit_id.slice(0, 12));
  return lines.join("\n");
}
