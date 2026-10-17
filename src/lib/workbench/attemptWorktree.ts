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
import { repoStore, type MutationOutcome, type MutationTarget, type ResolvedRepo } from "../stores/repoStore";
import type { MergeTeardownResult, WorktreeInfo } from "../branches/types";
import { MergeTeardownFailure, readMergeTeardownError } from "../branches/mergeTeardown";
import { familyFromCommonDir } from "../repos/repoFamily";
import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
import { formatError } from "../ui/formatError";
import { plural } from "../format";
import { isAttemptWorktree } from "./taskSessions";
import { runHoldsCheckout } from "./taskHandoff";
import { listAllLiveRuns, type TaskRun } from "./client";
import { liveRunIn } from "./liveRun";

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
  const list = await invoke<WorktreeInfo[]>("cmd_list_worktrees", { repoPath: root, depth: "dirty" });
  const key = identityKey(cwd, opts);
  const own = list.find((entry) => identityKey(entry.path, opts) === key);
  if (!own) throw new Error(`${cwd} is no longer a worktree of ${root}. It may already have been merged or removed.`);
  return { main: list.find((entry) => entry.is_main) ?? null, own };
}

/**
 * The main checkout as a mutation target. A tab that already holds it is
 * looked up (`known`) before anything is asked, so closing it while the reader
 * decides refuses the run. When none holds it, its tab is opened in the
 * background only now, after approval and just before the work, so a declined
 * or refused action leaves no tab behind. The work runs there, counted by the
 * quit guard and journalled, whatever is in front.
 */
async function mainCheckoutTarget(root: string, known: MutationTarget | null): Promise<MutationTarget> {
  if (known) return known;
  const target = await repoStore.targetForPath(root);
  if (!target) throw new Error(`${root} could not be opened, so nothing is run in it.`);
  return target;
}

/** What a failed or refused mutation threw, or says, for a caller that reports it as an error. */
function failureOf(outcome: MutationOutcome): unknown {
  return outcome.cause ?? new Error(outcome.error ?? "It did not complete.");
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
  const known = await repoStore.targetForPath(root, { open: false });
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
  const mutationTarget = await mainCheckoutTarget(root, known);
  const merged = await mergeAndTeardown(mutationTarget, run.cwd, target, own.head);
  if (merged.worktree_removed) await closeStranded(run.cwd);
  const parts = [mergedSentence(merged)];
  if (merged.worktree_removed && !merged.branch_deleted) parts.push(`The branch ${merged.merged_branch} was kept: ${merged.branch_error ?? "no reason was given"}`);
  if (merged.cleanup_error) parts.push(merged.cleanup_error);
  if (merged.sweep_problems?.length) parts.push(`Leftovers of earlier removals were not cleared: ${merged.sweep_problems.join("; ")}`);
  if (merged.hook_error) parts.push(`The post-merge hook failed: ${merged.hook_error}`);
  return { message: parts.join(" ") };
}

/**
 * The one call to the host's merge-teardown, through the store so it is
 * counted, queued and journalled. `expectedTip` is the commit the caller
 * verified the branch at, and the host merges nothing when the branch is
 * anywhere else once it holds its lock. The host's typed rejection
 * (`{code, message, ...}`) is kept as a `MergeTeardownFailure`, so a caller
 * can tell a conflict from a refusal; any other rejection passes through, and
 * so does the store's refusal when the repository changed while it waited.
 */
async function mergeAndTeardown(mutationTarget: MutationTarget, cwd: string, target: string, expectedTip: string): Promise<MergeTeardownResult> {
  const outcome = await repoStore.worktreeMergeTeardown({ worktreePath: cwd, targetBranch: target, squash: false, expectedTip }, mutationTarget, true);
  if (outcome.ok && outcome.output) return outcome.output;
  const cause = failureOf(outcome);
  const failure = readMergeTeardownError(cause);
  throw failure ? new MergeTeardownFailure(failure) : cause;
}

/** What landed, and, when the cleanup after it did not finish, that it is pending and why. */
function mergedSentence(merged: MergeTeardownResult): string {
  const landed = `Merged ${plural(merged.commits_merged, "commit")} from ${merged.merged_branch} into ${merged.target_branch}`;
  if (!merged.worktree_removed) return `${landed}; cleanup pending: ${merged.removal_error ?? "the worktree was not removed"}.`;
  return `${landed}. The worktree was removed.`;
}

/** What an unattended merge did: the commit it merged and the line the board shows. */
export interface VerifiedMerge { head: string; message: string; beforeOid: string; afterOid: string | null }

/**
 * Merges an attempt's branch for a caller that already holds the person's
 * consent (a wave they started) and an evidence-gated verdict, so it asks
 * nothing — and therefore checks everything the dialog's reader would have
 * looked at. It refuses, and merges nothing, when the worktree's head is not
 * `expectedHead` (the commit the verdict was reached for), when the worktree
 * has uncommitted files (teardown would lose them, and no verdict saw them),
 * when the main checkout is dirty or unreadable (a person's or another
 * session's work), or when a live attempt is working in the main checkout.
 * It never pushes: the host command it calls only merges and tears down.
 */
export async function mergeVerifiedAttempt(run: AttemptRef, expectedHead: string): Promise<VerifiedMerge> {
  if (!expectedHead) throw new Error("No verified head was named, so nothing is merged.");
  const root = await mainCheckoutOf(run.cwd);
  const known = await repoStore.targetForPath(root, { open: false });
  const { main, own } = await listingFor(root, run.cwd);
  if (!own.branch) throw new Error("This worktree is not on a branch, so there is nothing to merge.");
  if (own.head !== expectedHead) throw new Error(`The worktree moved to ${own.head.slice(0, 8)} after ${expectedHead.slice(0, 8)} was verified, so it is not merged.`);
  if (own.dirty_files !== 0) throw new Error(own.dirty_files === null ? "The worktree's uncommitted changes could not be counted, so it is not merged." : `The worktree has ${plural(own.dirty_files, "uncommitted file")} no verdict covered, so it is not merged.`);
  const target = main?.branch ?? null;
  if (!main || !target) throw new Error("The main checkout is not on a branch, so there is nothing to merge into.");
  if (main.dirty_files !== 0) throw new Error(main.dirty_files === null ? `The main checkout ${root} could not be checked for uncommitted work, so nothing is merged into it.` : `The main checkout ${root} has ${plural(main.dirty_files, "uncommitted file")}, which are not this wave's to merge over.`);
  const live = await listAllLiveRuns();
  if (!live.complete) throw new Error("Live attempts could not all be read, so the main checkout cannot be shown free of other sessions.");
  if (liveRunIn(root, live.runs)) throw new Error(`Another attempt is working in the main checkout ${root}, so nothing is merged into it.`);
  if (!(await repoStore.trustRepo(run.cwd))) throw new Error("The repository is not trusted, so it is not merged.");
  // The head the verdict was reached for is the tip the host holds the merge to.
  const mutationTarget = await mainCheckoutTarget(root, known);
  const merged = await mergeAndTeardown(mutationTarget, run.cwd, target, expectedHead);
  if (merged.worktree_removed) await closeStranded(run.cwd);
  const parts = [mergedSentence(merged)];
  if (merged.worktree_removed && !merged.branch_deleted) parts.push(`The branch ${merged.merged_branch} was kept: ${merged.branch_error ?? "no reason was given"}`);
  if (merged.cleanup_error) parts.push(merged.cleanup_error);
  if (merged.sweep_problems?.length) parts.push(`Leftovers of earlier removals were not cleared: ${merged.sweep_problems.join("; ")}`);
  if (merged.hook_error) parts.push(`The post-merge hook failed: ${merged.hook_error}`);
  // Where the target stood before and after: what a re-verify of the merged checkout is judged over.
  return { head: expectedHead, message: parts.join(" "), beforeOid: merged.before_oid, afterOid: merged.after_oid };
}

/**
 * Removes the attempt's worktree, and its `gitpulse/…` branch when Git will
 * delete it without force. Asks first, naming how many uncommitted files are
 * lost; forces the removal only when that count was read and confirmed, and
 * never forces the branch: one with commits nothing merged is kept and said.
 */
export async function discardAttemptWorktree(run: AttemptRef): Promise<WorktreeActionResult> {
  const root = await mainCheckoutOf(run.cwd);
  const known = await repoStore.targetForPath(root, { open: false });
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
  const mutationTarget = await mainCheckoutTarget(root, known);
  const removed = await repoStore.removeWorktree(run.cwd, force, mutationTarget, true);
  if (!removed.ok) throw failureOf(removed);
  await closeStranded(run.cwd);
  const parts = ["The worktree was removed."];
  if (removed.output?.cleanup_error) parts.push(removed.output.cleanup_error);
  if (removed.output?.sweep_problems?.length) parts.push(`Leftovers of earlier removals were not cleared: ${removed.output.sweep_problems.join("; ")}`);
  if (own.branch?.startsWith("gitpulse/")) {
    const deleted = await repoStore.deleteBranch(own.branch, false, mutationTarget, true);
    parts.push(deleted.ok ? `Its branch ${own.branch} was deleted.` : `Its branch ${own.branch} was kept: ${deleted.error ?? "it did not complete."}`);
  } else if (own.branch) {
    parts.push(`Its branch ${own.branch} was kept.`);
  }
  return { message: parts.join(" ") };
}
