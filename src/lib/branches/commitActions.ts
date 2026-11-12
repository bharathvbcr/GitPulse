/**
 * What can be done to the commit on screen: put a branch on it, or move the
 * current branch to it. One owner for both.
 *
 * They lived twice. The commit graph's row menu created a branch at a commit,
 * and HEAD moves had its own branch-here and reset-here buttons on every row.
 * Graph's three lists now share one details pane, so the actions sit there
 * (and in the row menu) and act on whichever commit the pane describes,
 * whichever list selected it.
 *
 * Both name their repository before any prompt (`captureTarget`), so a tab
 * switch while a dialog is open cannot redirect the change, and both report a
 * failure the store did not already surface.
 */
import { get } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { repoStore } from "../stores/repoStore";
import { askConfirm, askText } from "../stores/modalStore";
import { toastStore } from "../stores/toastStore";
import { reportFailure } from "../stores/mutationOutcome";
import { formatError } from "../ui/formatError";
import { plural, shortHash } from "../format";
import { describeResetPreview } from "./resetPreview";
import { historySelectionLabel } from "../views/historyList";
import type { ResetPreview } from "./types";

/**
 * A new branch at `commitId`. The non-destructive way back to a commit a
 * reset or amend left behind, and the plain way to start work from one.
 * Resolves true when the branch was created.
 */
export async function createBranchAt(commitId: string, label?: string): Promise<boolean> {
  if (!commitId) return false;
  const target = repoStore.captureTarget();
  const name = await askText({
    title: "Create Branch Here",
    message: `New branch at ${shortHash(commitId, 8)}${label ? ` (${label})` : ""}`,
    placeholder: "feat/new-branch",
    confirmLabel: "Create Branch",
  });
  const trimmed = name?.trim();
  if (!trimmed) return false;
  const outcome = await repoStore.createBranch(trimmed, commitId, target);
  if (!reportFailure(outcome, "Couldn't create the branch")) return false;
  toastStore.success(`Created branch "${trimmed}" at ${shortHash(commitId)}`);
  return true;
}

/**
 * Moves the current branch to `commitId` with `reset --keep`.
 *
 * The preview is read first so the confirmation names every commit the
 * branch would drop — and how many of them nothing else still reaches.
 * `--keep` never destroys uncommitted work: git refuses the reset when a
 * local change would be overwritten, and that refusal is what is shown. The
 * reset goes to the oid that was previewed, not to a name that could have
 * moved meanwhile. Resolves true when the branch moved.
 */
export async function resetKeepTo(commitId: string, label?: string): Promise<boolean> {
  const repo = get(repoStore).currentPath;
  if (!repo || !commitId) return false;
  const target = repoStore.captureTarget();
  let preview: ResetPreview;
  try {
    preview = await invoke<ResetPreview>("cmd_reset_preview", { repoPath: repo, target: commitId });
  } catch (err) {
    toastStore.error(`Couldn't preview the reset: ${formatError(err)}`);
    return false;
  }
  // The preview belongs to the repository it was read from.
  if (repo !== get(repoStore).currentPath) return false;
  const described = describeResetPreview(preview, label);
  const confirmed = await askConfirm({
    title: described.title,
    message: described.message,
    confirmLabel: "Reset",
    destructive: preview.leaving_total > 0,
  });
  if (!confirmed) return false;
  const outcome = await repoStore.resetTo("keep", preview.target, target);
  if (!reportFailure(outcome, "Couldn't reset")) return false;
  // HEAD moved, so every reflog selector shifted: the label the details pane
  // would put in the next prompt (`HEAD@{2}`) now names a different entry.
  historySelectionLabel.set(null);
  toastStore.success(
    `${preview.branch ?? "HEAD"} reset to ${shortHash(preview.target)}` +
      (preview.leaving_total > 0 ? ` — ${plural(preview.leaving_total, "commit")} left the branch` : ""),
  );
  return true;
}
