<script lang="ts">
  /**
   * One pull request, read with `gh pr view`, and the three things a reader
   * does to it from here: approve, comment / request changes, merge.
   *
   * Every action goes through `confirmAndRunPrAction`, which names what will
   * be published before sending it. A merge is pinned to the head commit this
   * view loaded (`--match-head-commit`), so a push made after the reader
   * looked makes gh refuse instead of merging what was never shown.
   */
  import { invoke } from "../../ipc/invoke";
  import { formatError } from "../../ui/formatError";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { verdictLabel } from "../../stores/harnessStore";
  import { confirmAndRunPrAction } from "../../github/prActions";
  import type { PrMergeMethod, PrReviewVerdict, PullRequestDetail } from "../../github/types";
  import { shortHash } from "../../format";

  let {
    repoPath,
    number,
    slug,
    onChanged,
  }: {
    repoPath: string;
    number: number;
    slug: string;
    /** Called after an action succeeded, so the list can reload. */
    onChanged: () => void;
  } = $props();

  let detail = $state<PullRequestDetail | null>(null);
  let loadError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let reviewBody = $state("");
  let mergeMethod = $state<PrMergeMethod>("squash");
  let deleteBranch = $state(false);
  let inflight: AsyncGuard | null = null;

  async function load() {
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loadError = null;
    try {
      const next = await invoke<PullRequestDetail>("cmd_github_pr_view", { repoPath, number });
      if (guard.isLive()) detail = next;
    } catch (error) {
      if (guard.isLive()) loadError = formatError(error);
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
    notice = null;
    try {
      const result = await confirmAndRunPrAction(repoPath, action, slug, { pr: detail });
      if (!result) return;
      notice = `${result.output || done}${result.policy ? ` — ${verdictLabel(result.policy)}` : ""}`;
      reviewBody = "";
      onChanged();
      await load();
    } catch (error) {
      actionError = formatError(error);
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

<div data-pr-detail={number} class="mt-2 p-3 rounded-xl border border-border/60 bg-background/60 text-xs space-y-3">
  {#if loadError}
    <div class="text-rose-400">Could not load #{number}: {loadError}</div>
  {:else if !detail}
    <div class="text-textMuted">Loading #{number}…</div>
  {:else}
    <div class="flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-textMuted font-mono">
      <span>by {detail.author || "unknown"}</span>
      <span>{detail.head_ref} → {detail.base_ref} @ {shortHash(detail.head_oid)}</span>
      <span><span class="text-emerald-500">+{detail.additions}</span> <span class="text-rose-400">−{detail.deletions}</span> in {detail.changed_files} files</span>
      <span>CI {detail.ci_status}</span>
      <span>{detail.mergeable.toLowerCase()} · {detail.merge_state.toLowerCase()}</span>
      {#if detail.review_decision}<span>{detail.review_decision.toLowerCase().replace("_", " ")}</span>{/if}
      {#if !open}<span class="gp-pill">{detail.state.toLowerCase()}</span>{/if}
    </div>
    {#if detail.body.trim()}
      <div class="max-h-48 overflow-auto whitespace-pre-wrap text-textPrimary/90 font-sans leading-relaxed">{detail.body}</div>
      {#if detail.body_truncated}
        <div class="text-[11px] text-amber-600 dark:text-amber-400">The description is longer than shown here; open it on GitHub for the rest.</div>
      {/if}
    {:else}
      <div class="text-textMuted italic">No description.</div>
    {/if}

    {#if open}
      <div class="space-y-1.5">
        <textarea
          class="gp-field w-full min-h-16 font-sans"
          placeholder="Review comment (required to comment or request changes)"
          aria-label="Review comment for #{number}"
          bind:value={reviewBody}
          disabled={busy}
        ></textarea>
        <div class="flex flex-wrap gap-1.5">
          <button type="button" class="gp-btn py-1! px-2.5! text-[11px]!" disabled={busy} onclick={() => review("approve")}>Approve…</button>
          <button type="button" class="gp-btn py-1! px-2.5! text-[11px]!" disabled={busy || !reviewBody.trim()} onclick={() => review("comment")}>Comment…</button>
          <button type="button" class="gp-btn py-1! px-2.5! text-[11px]!" disabled={busy || !reviewBody.trim()} onclick={() => review("request_changes")}>Request changes…</button>
        </div>
      </div>
      <div class="flex flex-wrap items-center gap-2 pt-1 border-t border-border/50">
        <label class="flex items-center gap-1.5 text-[11px] text-textMuted">
          Merge as
          <select class="gp-field py-0.5! text-[11px]!" bind:value={mergeMethod} disabled={busy} aria-label="Merge method for #{number}">
            <option value="squash">squash</option>
            <option value="merge">merge commit</option>
            <option value="rebase">rebase</option>
          </select>
        </label>
        <label class="flex items-center gap-1.5 text-[11px] text-textMuted">
          <input type="checkbox" bind:checked={deleteBranch} disabled={busy} />
          delete branch
        </label>
        <button
          type="button"
          class="gp-btn py-1! px-2.5! text-[11px]! text-rose-300"
          disabled={busy || detail.is_draft}
          title={detail.is_draft ? "A draft cannot be merged" : `Merge, pinned to ${shortHash(detail.head_oid)}`}
          onclick={merge}
        >Merge…</button>
      </div>
    {/if}
  {/if}
  {#if notice}<div class="text-emerald-600 dark:text-emerald-400" role="status">{notice}</div>{/if}
  {#if actionError}<div class="text-rose-400" role="alert">{actionError}</div>{/if}
</div>
