/**
 * What to do with an attempt's worktree once its agent has stopped: review
 * what it changed, merge it into the repository's main checkout, or discard
 * it.
 *
 * Every one of these existed — `previewUncommitted`, the Worktrees panel's
 * merge-and-teardown and remove — but none was reachable from the attempt
 * that made the worktree, so finishing an attempt meant finding its folder in
 * another panel by name. They are offered on the attempt's row now, and only
 * when it no longer holds its checkout: the host refuses a merge or removal
 * while a live run is working there anyway, and an offer the host will
 * refuse is a dead end.
 *
 * The repository a merge or removal runs against is the attempt's *main
 * checkout* — the parent of the common Git directory every worktree of the
 * repository shares (`repoFamily.ts::familyFromCommonDir`). Never the active
 * repository: the reader may be looking at anything, including another
 * worktree, while the task sheet is open.
 */

import { get } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { interfaceStore } from "../stores/interfaceStore";
import { askConfirm } from "../stores/modalStore";
import { repoStore, type ResolvedRepo } from "../stores/repoStore";
import type { Guarded } from "../stores/harnessStore";
import type { MergeTeardownResult, WorktreeInfo } from "../branches/types";
import { familyFromCommonDir } from "../repos/repoFamily";
import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
import { formatError } from "../ui/formatError";
import { plural } from "../format";
import { isAttemptWorktree } from "./taskSessions";
import { runHoldsCheckout } from "./taskHandoff";
import type { TaskRun } from "./client";

type AttemptRef = Pick<TaskRun, "id" | "cwd" | "state" | "expires_at">;

/** Which after-run actions an attempt's row offers right now. */
export interface AttemptWorktreeOffer {
  /** Its checkout's uncommitted changes, in the repository view. */
  review: boolean;
  /** Merge or discard: only the attempt's own worktree, never a shared checkout. */
  ownWorktree: boolean;
}

export function attemptWorktreeOffer(run: AttemptRef, clock: number): AttemptWorktreeOffer {
  if (runHoldsCheckout(run, clock)) return { review: false, ownWorktree: false };
  return { review: true, ownWorktree: isAttemptWorktree(run.cwd, run.id) };
}

function options() {
  return { caseInsensitive: isCaseInsensitiveFs() };
}

/**
 * The main checkout of the repository `cwd` belongs to: the family root of
 * an open tab on it, else of a fresh resolve. Throws when it cannot be told,
 * or when `cwd` is the main checkout itself (there is no worktree to act on).
 */
export async function mainCheckoutOf(cwd: string): Promise<string> {
  const opts = options();
  const key = identityKey(cwd, opts);
  const open = key ? get(repoStore).openTabs.find((tab) => identityKey(tab.path, opts) === key) : undefined;
  let root = open?.familyRoot ?? null;
  if (!root) {
    let resolved: ResolvedRepo;
    try {
      resolved = await invoke<ResolvedRepo>("cmd_resolve_repo", { repoPath: cwd });
    } catch (cause) {
      throw new Error(`${cwd} could not be read as a Git worktree: ${formatError(cause)}`);
    }
    root = familyFromCommonDir(resolved.common_dir, opts)?.root ?? null;
  }
  if (!root) throw new Error(`GitPulse could not tell which repository ${cwd} belongs to, so it will not merge or remove it.`);
  if (identityKey(root, opts) === key) throw new Error("This attempt ran in the repository's main checkout, so there is no worktree to merge or discard.");
  return root;
}

/** The main checkout's branch, from its open tab, for the button's label. Null when unknown. */
export function mergeTargetLabel(cwd: string): string | null {
  const opts = options();
  const key = identityKey(cwd, opts);
  const tabs = get(repoStore).openTabs;
  const root = key ? tabs.find((tab) => identityKey(tab.path, opts) === key)?.familyRoot : null;
  const rootKey = root ? identityKey(root, opts) : "";
  return rootKey ? tabs.find((tab) => identityKey(tab.path, opts) === rootKey)?.currentBranch ?? null : null;
}

/** The main checkout's entry and the attempt's own, from one listing. */
async function listingFor(root: string, cwd: string): Promise<{ main: WorktreeInfo | null; own: WorktreeInfo }> {
  const opts = options();
  const list = await invoke<WorktreeInfo[]>("cmd_list_worktrees", { repoPath: root });
  const key = identityKey(cwd, opts);
  const own = list.find((entry) => identityKey(entry.path, opts) === key);
  if (!own) throw new Error(`${cwd} is no longer a worktree of ${root}. It may already have been merged or removed.`);
  return { main: list.find((entry) => entry.is_main) ?? null, own };
}

/** What a removal would cost, said before it happens. */
function changesClause(dirty: number | null): string {
  if (dirty === null) return "Its uncommitted changes could not be counted, so only a clean removal is tried.";
  return dirty > 0 ? `${plural(dirty, "uncommitted file")} in it will be lost.` : "It has no uncommitted changes.";
}

async function closeStranded(path: string): Promise<void> {
  const opts = options();
  const key = identityKey(path, opts);
  const stranded = key ? get(repoStore).openTabs.find((tab) => identityKey(tab.path, opts) === key) : undefined;
  if (stranded) await repoStore.closeTab(stranded.id);
}

/**
 * Opens the attempt's checkout on its uncommitted changes, in the
 * repository view — the reader asked to go there.
 */
export async function reviewAttemptChanges(run: Pick<TaskRun, "cwd">): Promise<void> {
  await repoStore.previewUncommitted(run.cwd);
  interfaceStore.setGlobalSurface("repository");
}

/** The outcome of an after-run action, as the row says it. `null`: the reader declined. */
export type WorktreeActionResult = { message: string } | null;

/**
 * Merges the attempt's branch into the main checkout's branch and removes
 * the worktree, through the same host command the Worktrees panel uses.
 * Asks first, naming the target and what uncommitted work the removal loses.
 */
export async function mergeAttemptWorktree(run: AttemptRef): Promise<WorktreeActionResult> {
  const root = await mainCheckoutOf(run.cwd);
  const { main, own } = await listingFor(root, run.cwd);
  if (!own.branch) throw new Error("This worktree is not on a branch, so there is nothing to merge.");
  const target = main?.branch ?? null;
  if (!target) throw new Error("The main checkout is not on a branch, so there is nothing to merge into.");
  const approved = await askConfirm({
    title: `Merge into ${target}?`,
    message: `Merges ${own.branch} into ${target} in ${root}, then removes the worktree at ${run.cwd} and its branch. ${changesClause(own.dirty_files)}`,
    confirmLabel: `Merge into ${target}`,
    destructive: (own.dirty_files ?? 0) > 0,
  });
  if (!approved) return null;
  if (!(await repoStore.trustRepo(run.cwd))) return null;
  const result = await invoke<Guarded<MergeTeardownResult>>("cmd_worktree_merge_teardown", {
    repoPath: root,
    worktreePath: run.cwd,
    targetBranch: target,
    squash: false,
  });
  await closeStranded(run.cwd);
  const merged = result.output;
  const parts = [`Merged ${plural(merged.commits_merged, "commit")} from ${merged.merged_branch} into ${merged.target_branch}.`];
  parts.push(merged.worktree_removed ? "The worktree was removed." : "The worktree was not removed.");
  if (!merged.branch_deleted) parts.push(`The branch ${merged.merged_branch} was kept.`);
  if (merged.hook_error) parts.push(`The post-merge hook failed: ${merged.hook_error}`);
  return { message: parts.join(" ") };
}

/**
 * Removes the attempt's worktree, and its `gitpulse/…` branch when Git will
 * delete it without force. Asks first, naming how many uncommitted files are
 * lost; forces the removal only when that count was read and confirmed, and
 * never forces the branch: one with commits nothing merged is kept and said.
 */
export async function discardAttemptWorktree(run: AttemptRef): Promise<WorktreeActionResult> {
  const root = await mainCheckoutOf(run.cwd);
  const { own } = await listingFor(root, run.cwd);
  const dirty = own.dirty_files;
  const approved = await askConfirm({
    title: "Discard this worktree?",
    message: `Removes the worktree at ${run.cwd}. ${changesClause(dirty)}${own.branch ? ` Its branch ${own.branch} is deleted only if it has no unmerged commits.` : ""}`,
    confirmLabel: dirty !== null && dirty > 0 ? `Discard ${plural(dirty, "file")}` : "Discard worktree",
    destructive: true,
  });
  if (!approved) return null;
  if (!(await repoStore.trustRepo(run.cwd))) return null;
  const force = dirty !== null && dirty > 0;
  await invoke("cmd_remove_worktree", { repoPath: root, targetPath: run.cwd, force });
  await closeStranded(run.cwd);
  const parts = ["The worktree was removed."];
  if (own.branch?.startsWith("gitpulse/")) {
    try {
      await invoke("cmd_delete_branch", { repoPath: root, branchName: own.branch, force: false });
      parts.push(`Its branch ${own.branch} was deleted.`);
    } catch (cause) {
      parts.push(`Its branch ${own.branch} was kept: ${formatError(cause)}`);
    }
  } else if (own.branch) {
    parts.push(`Its branch ${own.branch} was kept.`);
  }
  return { message: parts.join(" ") };
}
