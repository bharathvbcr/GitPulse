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
import { askChoice, askConfirm } from "../stores/modalStore";
import { repoStore, type MutationOutcome, type MutationTarget, type ResolvedRepo } from "../stores/repoStore";
import type { MergeTeardownResult, WorktreeInfo } from "../branches/types";
import { MergeTeardownFailure, readMergeTeardownError } from "../branches/mergeTeardown";
import { MERGE_MODES } from "../branches/mergeSelection";
import { familyFromCommonDir } from "../repos/repoFamily";
import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
import { formatError } from "../ui/formatError";
import { plural } from "../format";
import { isAttemptWorktree } from "./taskSessions";
import { runHoldsCheckout } from "./taskHandoff";
import { listAllLiveRuns, type TaskRun } from "./client";
import { liveRunIn } from "./liveRun";
import { readAttemptReview, type AttemptReview } from "./attemptReview";
import { toastStore } from "../stores/toastStore";
import type { ProvenanceFreshness } from "../provenance/types";
import { ATTEMPT_BRANCH_PREFIX, squashesByDefault } from "./attemptBranch";

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
export async function reviewAttemptUncommitted(run: Pick<TaskRun, "cwd">): Promise<void> {
  await repoStore.previewUncommitted(run.cwd);
  interfaceStore.setGlobalSurface("repository");
}

/** How many files the checkout holds uncommitted, from the same listing a merge reads; null when it was not counted. */
export async function attemptDirtyFiles(run: Pick<TaskRun, "cwd">): Promise<number | null> {
  const root = await mainCheckoutOf(run.cwd);
  return (await listingFor(root, run.cwd)).own.dirty_files;
}

/** What opening an attempt's committed range left the repository view showing. */
export interface AttemptDiffOpening {
  /** `range`: the committed range was selected in the attempt's checkout. `none`: it was not, and a notice says why. */
  shown: "range" | "none";
  from: string;
  to: string;
  /** What the reader must know before relying on the diff; empty only when nothing qualifies it. */
  notices: string[];
  /** The range diff is a prefix: the host cut it at its read budget. */
  truncated: boolean;
}

const short = (oid: string) => oid.slice(0, 12);

/** How long a notice about a diff stays up: long enough to read before approving. */
const NOTICE_MS = 15_000;

type Ancestry = { kind: "ancestor" } | { kind: "diverged" } | { kind: "unknown"; reason: string };

/**
 * Whether `base` is an ancestor of `head`, from the host's own measurement
 * (the review gate's `merge-base --is-ancestor`), in the attempt's checkout.
 * The reply is `landed_on_base` for `base` measured against `head`: true is
 * an ancestor, false is measured not-an-ancestor, null is not measured.
 */
async function baseAncestry(cwd: string, base: string, head: string): Promise<Ancestry> {
  try {
    const row = await invoke<ProvenanceFreshness>("cmd_provenance_freshness", { repoPath: cwd, commitSha: base, baseBranch: head });
    if (row.landed_on_base === true) return { kind: "ancestor" };
    if (row.landed_on_base === false) return { kind: "diverged" };
    return { kind: "unknown", reason: row.base_relation_reason || "the host gave no reason" };
  } catch (cause) {
    return { kind: "unknown", reason: formatError(cause) };
  }
}

/**
 * Selects `from...to` in the attempt's own checkout. The store's range
 * selection has no repository argument and diffs whichever tab is active, so
 * the checkout is brought forward and awaited first: selecting at once would
 * diff whatever is in front. False when the checkout never became ready.
 */
async function selectRangeInCheckout(cwd: string, from: string, to: string): Promise<boolean> {
  const opts = options();
  const here = get(repoStore);
  if (here.currentPath && identityKey(here.currentPath, opts) === identityKey(cwd, opts) && !here.isLoading && !here.error) {
    await repoStore.selectRangeDiff(from, to);
    return true;
  }
  let selecting: Promise<void> | undefined;
  await repoStore.openRepo(cwd, { onReady: () => { selecting = repoStore.selectRangeDiff(from, to); } });
  if (!selecting) return false;
  await selecting;
  return true;
}

/**
 * Opens the range an attempt committed (`base_oid` to `head_oid` of its
 * review record) in its checkout, in the repository view, and says what the
 * reader cannot see for themselves. The range diff is three-dot while the
 * review gate binds the two-dot range, and the two agree only while base is
 * an ancestor of head; the diff is a prefix when it is cut at the read budget;
 * and a selection can be overtaken. Each is a notice, here and as a warning
 * toast where the reader lands, and none is dropped on the way to "shown".
 */
export async function openAttemptDiff(
  run: Pick<TaskRun, "cwd">,
  review: Pick<AttemptReview, "base_oid" | "head_oid" | "files_changed">,
): Promise<AttemptDiffOpening> {
  const { base_oid: from, head_oid: to } = review;
  const notices: string[] = [];
  const ready = await selectRangeInCheckout(run.cwd, from, to);
  interfaceStore.setGlobalSurface("repository");
  let truncated = false;
  let shown: AttemptDiffOpening["shown"] = "none";
  if (!ready) {
    notices.push(`The attempt's checkout did not finish opening, so its changes are not shown. Open it from the task and try again.`);
  } else {
    const opts = options();
    const view = get(repoStore);
    const here = view.currentPath !== null && identityKey(view.currentPath, opts) === identityKey(run.cwd, opts) && view.selectionKind === "range" && view.selectedFilePath === `${from}...${to}`;
    if (!here) {
      notices.push("The repository view moved on before this attempt's changes were selected, so it may not be showing them. Use View diff again.");
    } else if (view.selectedDiffError) {
      notices.push(`The diff could not be read: ${view.selectedDiffError}`);
    } else {
      shown = "range";
      if (view.selectedDiffTruncated) {
        truncated = true;
        const why = view.selectedDiffTruncationReason ? ` (${view.selectedDiffTruncationReason})` : "";
        notices.push(`Only part of this diff is shown${why}. The review covers all ${plural(review.files_changed, "file")} in ${short(from)}..${short(to)}, not just what is on screen.`);
      }
    }
    // Asked only once the checkout is open: the host refuses an untrusted one.
    const ancestry = await baseAncestry(run.cwd, from, to);
    if (ancestry.kind === "diverged") {
      notices.push(`${short(from)} is not an ancestor of ${short(to)}, so this diff (${short(from)}...${short(to)}, from their merge base) is not the range the review covers (${short(from)}..${short(to)}). Check that range itself before approving.`);
    } else if (ancestry.kind === "unknown") {
      notices.push(`Whether ${short(from)} is an ancestor of ${short(to)} could not be told (${ancestry.reason}), so this diff may not be the range the review covers.`);
    }
  }
  for (const notice of notices) toastStore.add({ kind: "warning", message: notice, duration: NOTICE_MS });
  return { shown, from, to, notices, truncated };
}

/**
 * Opens what the attempt did, in the repository view: the range it committed
 * against its base when it committed anything, else its uncommitted files.
 * When the commits cannot be read it shows the uncommitted files and says the
 * commits are missing from the view, rather than an empty review that looks
 * like an attempt that did nothing.
 */
export async function reviewAttemptChanges(run: Pick<TaskRun, "id" | "cwd">): Promise<void> {
  let review: AttemptReview;
  try {
    review = await readAttemptReview(run.id);
  } catch (cause) {
    toastStore.add({ kind: "warning", message: `This attempt's commits could not be read (${formatError(cause)}), so only its uncommitted files are shown.`, duration: NOTICE_MS });
    return reviewAttemptUncommitted(run);
  }
  if (review.head_oid === review.base_oid) return reviewAttemptUncommitted(run);
  await openAttemptDiff(run, review);
}

/** The outcome of an after-run action, as the row says it. `null`: the reader declined. */
export type WorktreeActionResult = { message: string } | null;

const SQUASH_MODE = MERGE_MODES.find((mode) => mode.mode === "squash");

/**
 * Asks how to land `branch` into `target`: the one confirm the merge needs,
 * with squash as its other way (wording from `mergeSelection.ts`). The default
 * answer is the primary button. Resolves whether to squash, or null when the
 * reader declined.
 */
async function askHowToMerge(question: { branch: string; target: string; detail: string; destructive: boolean }): Promise<boolean | null> {
  if (!SQUASH_MODE) throw new Error("The squash merge mode is missing from the merge options.");
  const defaultSquash = squashesByDefault(question.branch);
  const plain = `Merge into ${question.target}`;
  const answer = await askChoice({
    title: `Merge into ${question.target}?`,
    message: `Merges ${question.detail}\n${SQUASH_MODE.label}: ${SQUASH_MODE.detail}`,
    confirmLabel: defaultSquash ? SQUASH_MODE.label : plain,
    alternateLabel: defaultSquash ? plain : SQUASH_MODE.label,
    destructive: question.destructive,
  });
  if (answer === null) return null;
  return (answer === "confirm") === defaultSquash;
}

/**
 * Merges the attempt's branch into the main checkout's branch and removes
 * the worktree, through the same host command the Worktrees panel uses.
 * Asks first, naming the target and what uncommitted work the removal loses.
 *
 * `expectedTip` is the commit a review approved: the landing is held to it,
 * refused before asking when the worktree has moved, and the host holds the
 * merge to it again once it has its lock. Without it the tip the listing
 * showed is what the host holds the merge to.
 */
export async function mergeAttemptWorktree(run: AttemptRef, options: { expectedTip?: string } = {}): Promise<WorktreeActionResult> {
  const root = await mainCheckoutOf(run.cwd);
  const known = await repoStore.targetForPath(root, { open: false });
  const { main, own } = await listingFor(root, run.cwd);
  if (!own.branch) throw new Error("This worktree is not on a branch, so there is nothing to merge.");
  if (options.expectedTip !== undefined && own.head !== options.expectedTip) {
    throw new Error(`The worktree moved to ${own.head.slice(0, 8)} after ${options.expectedTip.slice(0, 12)} was approved, so it is not landed. Review the new commits.`);
  }
  const target = main?.branch ?? null;
  if (!target) throw new Error("The main checkout is not on a branch, so there is nothing to merge into.");
  const squash = await askHowToMerge({
    branch: own.branch,
    target,
    detail: `${own.branch} into ${target} in ${root}, then removes the worktree at ${run.cwd} and its branch. ${changesClause(own.dirty_files)}`,
    destructive: (own.dirty_files ?? 0) > 0,
  });
  if (squash === null) return null;
  if (!(await repoStore.trustRepo(run.cwd))) return null;
  const mutationTarget = await mainCheckoutTarget(root, known);
  const merged = await mergeAndTeardown(mutationTarget, run.cwd, target, options.expectedTip ?? own.head, squash);
  if (merged.worktree_removed) await closeStranded(run.cwd);
  const parts = [mergedSentence(merged, squash)];
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
async function mergeAndTeardown(mutationTarget: MutationTarget, cwd: string, target: string, expectedTip: string, squash: boolean): Promise<MergeTeardownResult> {
  const outcome = await repoStore.worktreeMergeTeardown({ worktreePath: cwd, targetBranch: target, squash, expectedTip }, mutationTarget, true);
  if (outcome.ok && outcome.output) return outcome.output;
  const cause = failureOf(outcome);
  const failure = readMergeTeardownError(cause);
  throw failure ? new MergeTeardownFailure(failure) : cause;
}

/** What landed, and, when the cleanup after it did not finish, that it is pending and why. */
function mergedSentence(merged: MergeTeardownResult, squash: boolean): string {
  const landed = squash
    ? `Squashed ${plural(merged.commits_merged, "commit")} from ${merged.merged_branch} into ${merged.target_branch} as one commit`
    : `Merged ${plural(merged.commits_merged, "commit")} from ${merged.merged_branch} into ${merged.target_branch}`;
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
  // An unattended wave merge keeps the commits as the verdict saw them.
  const merged = await mergeAndTeardown(mutationTarget, run.cwd, target, expectedHead, false);
  if (merged.worktree_removed) await closeStranded(run.cwd);
  const parts = [mergedSentence(merged, false)];
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
  if (own.branch?.startsWith(ATTEMPT_BRANCH_PREFIX)) {
    const deleted = await repoStore.deleteBranch(own.branch, false, mutationTarget, true);
    parts.push(deleted.ok ? `Its branch ${own.branch} was deleted.` : `Its branch ${own.branch} was kept: ${deleted.error ?? "it did not complete."}`);
  } else if (own.branch) {
    parts.push(`Its branch ${own.branch} was kept.`);
  }
  return { message: parts.join(" ") };
}
