import type { ResetPreview } from "./types";
import { plural, shortHash } from "../format";

/**
 * The confirmation a reflog reset shows, built from what the backend read.
 *
 * Every commit leaving the branch is named up to the preview's cap, and the
 * remainder is counted rather than dropped, so a capped list never reads as
 * the whole of what moves. The commits no other ref reaches are called out
 * separately: those are the ones that only the reflog will still hold.
 */
export function describeResetPreview(
  preview: ResetPreview,
  selector: string,
): { title: string; message: string } {
  const subject = preview.branch ? `branch "${preview.branch}"` : "detached HEAD";
  const lines = [
    `Move ${subject} from ${shortHash(preview.head)} to ${shortHash(preview.target)} (${selector}).`,
  ];
  if (preview.leaving_total === 0) {
    lines.push("No commits leave the branch.");
  } else {
    lines.push("", `${plural(preview.leaving_total, "commit")} would leave the branch:`);
    for (const commit of preview.leaving) {
      lines.push(`  ${shortHash(commit.commit_id)} ${commit.summary}`);
    }
    const unlisted = preview.leaving_total - preview.leaving.length;
    if (unlisted > 0) lines.push(`  …and ${unlisted} more`);
    lines.push(
      "",
      preview.unreachable_total > 0
        ? `${preview.unreachable_total} of them ${preview.unreachable_total === 1 ? "is" : "are"} on no other branch, tag or remote; afterwards only the reflog will hold ${preview.unreachable_total === 1 ? "it" : "them"}.`
        : "Every one of them is still reachable from another branch, tag or remote.",
    );
  }
  if (preview.gaining_total > 0) {
    lines.push("", `The branch would gain ${plural(preview.gaining_total, "commit")} it does not have now.`);
  }
  if (!preview.branch) {
    lines.push("", "HEAD is detached, so no branch moves — only HEAD.");
  }
  lines.push(
    "",
    "Uncommitted changes are kept (git reset --keep); git refuses the reset if one would be overwritten.",
  );
  return {
    title: preview.branch ? "Reset Branch Here" : "Move Detached HEAD Here",
    message: lines.join("\n"),
  };
}
