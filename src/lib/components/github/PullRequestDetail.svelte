<script lang="ts">
  import Checkbox from "../Checkbox.svelte";
  /**
   * One pull request, read with `gh pr view`, and the things a reader does
   * to it from here: open it in a new worktree, check it out in place,
   * approve, comment / request changes, merge.
   *
   * Published actions go through `confirmAndRunPrAction`, which names what
   * will be published before sending it. A merge is pinned to the head commit
   * this view loaded (`--match-head-commit`), so a push made after the reader
   * looked makes gh refuse instead of merging what was never shown.
   */
  import { invoke } from "../../ipc/invoke";
  import { describeFailure } from "../../ui/copy/describeFailure";
  import Skeleton from "../Skeleton.svelte";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { verdictLabel } from "../../stores/harnessStore";
  import {
    checkoutPullRequestInPlace,
    confirmAndRunPrAction,
    describePrWorktree,
    openPullRequestWorktree,
    withVerdict,
  } from "../../github/prActions";
  import type { PrMergeMethod, PrReviewVerdict, PullRequestDetail } from "../../github/types";
  import { shortHash } from "../../format";

  let {
    repoPath,
    number,
    slug,
    locked = false,
    onChanged,
    onCheckedOut,
    onWorktreeOpened,
    onBusyChange,
  }: {
    repoPath: string;
    number: number;
    slug: string;
    /** A list-row checkout or worktree open is already in flight. */
    locked?: boolean;
    /**
     * Called after an action succeeded, so the list can reload, with what the
     * action reported. The confirmation is the caller's to show: a merged pull
     * request leaves the list and takes this row with it, so a notice kept here
     * would vanish with the very thing it confirms.
     */
    onChanged: (message: string) => void;
    /** In-place checkout succeeded. The parent refreshes the open checkout. */
    onCheckedOut: (message: string) => void;
    /** A worktree was opened. The parent refreshes what lists worktrees; `onChanged` is used when absent. */
    onWorktreeOpened?: (message: string) => void;
    /** Tells the parent whether this row is mid-action, so the list rows lock too. */
    onBusyChange?: (busy: boolean) => void;
  } = $props();

  let detail = $state<PullRequestDetail | null>(null);
  let loadError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let busy = $state(false);
  let reviewBody = $state("");
  let mergeMethod = $state<PrMergeMethod>("squash");
  let deleteBranch = $state(false);
  let inflight: AsyncGuard | null = null;

  /** "Couldn't <verb> <object>. <Reason>" for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  $effect(() => {
    onBusyChange?.(busy);
    return () => onBusyChange?.(false);
  });

  async function load() {
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loadError = null;
    try {
      const next = await invoke<PullRequestDetail>("cmd_github_pr_view", { repoPath, number });
      if (guard.isLive()) detail = next;
    } catch (error) {
      if (guard.isLive()) loadError = failure("load", `#${number}`, error);
    }
  }

  $effect(() => {
    void number;
    void load();
    return () => inflight?.cancel();
  });

  const open = $derived(detail?.state === "OPEN");

  async function run(action: Parameters<typeof confirmAndRunPrAction>[1], done: string) {
    if (busy) return;
    busy = true;
    actionError = null;
    try {
      const result = await confirmAndRunPrAction(repoPath, action, slug, { pr: detail });
      if (!result) return;
      reviewBody = "";
      onChanged(`${result.output || done}${result.policy ? ` — ${verdictLabel(result.policy)}` : ""}`);
      // The list reload may drop this pull request and unmount the row; the
      // effect's teardown then cancels this read, so it is not wasted work.
      await load();
    } catch (error) {
      actionError = action.kind === "merge"
        ? failure("merge", `#${number}`, error)
        : failure("submit", "the review", error);
    } finally {
      busy = false;
    }
  }

  async function checkoutInPlace() {
    if (busy || locked) return;
    busy = true;
    actionError = null;
    try {
      const result = await checkoutPullRequestInPlace(repoPath, number);
      onCheckedOut(withVerdict(result.output || `Checked out pull request #${number} in place.`, result.policy));
    } catch (error) {
      actionError = failure("check out", `#${number}`, error);
    } finally {
      busy = false;
    }
  }

  async function openInWorktree() {
    if (busy || locked) return;
    busy = true;
    actionError = null;
    try {
      const result = await openPullRequestWorktree(repoPath, number);
      (onWorktreeOpened ?? onChanged)(withVerdict(describePrWorktree(result.output), result.policy));
    } catch (error) {
      actionError = failure("open", `#${number} in a worktree`, error);
    } finally {
      busy = false;
    }
  }

  function review(verdict: PrReviewVerdict) {
    void run({ kind: "review", number, verdict, body: reviewBody }, "Review submitted.");
  }

  function merge() {
    if (!detail) return;
    void run(
      { kind: "merge", number, method: mergeMethod, delete_branch: deleteBranch, head_oid: detail.head_oid },
      `Merged #${number}.`,
    );
  }
</script>

<div data-pr-detail={number} class="mt-2 p-3 rounded-xl border border-edge bg-background/60 text-xs space-y-3">
  {#if loadError}
    <div class="gp-notice" data-tone="danger" role="alert">
      <span class="min-w-0 flex-1">{loadError}</span>
      <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={() => void load()}>Try again</button>
    </div>
  {:else if !detail}
    <Skeleton variant="text" count={2} />
  {:else}
    <div class="flex flex-wrap gap-x-3 gap-y-1 text-meta text-textMuted font-mono">
      <span>by {detail.author || "unknown"}</span>
      <span>{detail.head_ref} → {detail.base_ref} @ {shortHash(detail.head_oid)}</span>
      <span><span class="text-ok">+{detail.additions}</span> <span class="text-danger">−{detail.deletions}</span> in {detail.changed_files} files</span>
      <span>CI {detail.ci_status}</span>
      <span>{detail.mergeable.toLowerCase()} · {detail.merge_state.toLowerCase()}</span>
      {#if detail.review_decision}<span>{detail.review_decision.toLowerCase().replace("_", " ")}</span>{/if}
      {#if !open}<span class="gp-chip" data-tone="neutral">{detail.state.toLowerCase()}</span>{/if}
    </div>
    {#if detail.body.trim()}
      <div class="max-h-48 overflow-auto whitespace-pre-wrap text-textPrimary/90 font-sans leading-relaxed">{detail.body}</div>
      {#if detail.body_truncated}
        <div class="text-meta text-warn">The description is longer than shown here; open it on GitHub for the rest.</div>
      {/if}
    {:else}
      <div class="text-textMuted italic">No description.</div>
    {/if}

    <div class="flex flex-wrap gap-1.5">
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
        disabled={busy || locked}
        onclick={() => void openInWorktree()}
      >Open in new worktree</button>
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
        disabled={busy || locked}
        onclick={() => void checkoutInPlace()}
      >Checkout in place</button>
    </div>

    {#if open}
      <div class="space-y-1.5">
        <textarea
          class="gp-field-multi w-full min-h-16 font-sans"
          placeholder="Review comment (required to comment or request changes)"
          aria-label="Review comment for #{number}"
          bind:value={reviewBody}
          disabled={busy || locked}
        ></textarea>
        <div class="flex flex-wrap gap-1.5">
          <button type="button" class="gp-btn" data-size="sm" disabled={busy || locked} onclick={() => review("approve")}>Approve…</button>
          <button type="button" class="gp-btn" data-size="sm" disabled={busy || locked || !reviewBody.trim()} onclick={() => review("comment")}>Comment…</button>
          <button type="button" class="gp-btn" data-size="sm" disabled={busy || locked || !reviewBody.trim()} onclick={() => review("request_changes")}>Request changes…</button>
        </div>
      </div>
      <div class="flex flex-wrap items-center gap-2 pt-3">
        <label class="flex items-center gap-1.5 text-meta text-textMuted">
          Merge as
          <select class="gp-select" bind:value={mergeMethod} disabled={busy || locked} aria-label="Merge method for #{number}">
            <option value="squash">squash</option>
            <option value="merge">merge commit</option>
            <option value="rebase">rebase</option>
          </select>
        </label>
        <label class="flex items-center gap-1.5 text-meta text-textMuted">
          <Checkbox ariaLabel="Delete branch" checked={deleteBranch} disabled={busy || locked} onchange={(next) => (deleteBranch = next)} />
          delete branch
        </label>
        <button
          type="button"
          class="gp-btn"
          data-variant="danger"
          data-size="sm"
          disabled={busy || locked || detail.is_draft}
          title={detail.is_draft ? "A draft cannot be merged" : `Merge, pinned to ${shortHash(detail.head_oid)}`}
          onclick={merge}
        >Merge…</button>
      </div>
    {/if}
  {/if}
  {#if actionError}<div class="gp-notice" data-tone="danger" role="alert">{actionError}</div>{/if}
</div>
