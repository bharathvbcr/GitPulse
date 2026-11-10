<script lang="ts" module>
  import type { BranchInfo } from "../branches/types";

  /**
   * What is known about the commits the current branch has not pushed.
   *
   * `unknown` is its own answer and never reads as "nothing to push": a branch
   * list that has not loaded, a detached HEAD, a branch with no upstream (git
   * reports no ahead count without one) and an upstream that is gone all leave
   * the count unmeasured, and the card says so instead of hiding.
   */
  export type OutgoingState =
    | { kind: "unknown"; reason: string }
    | { kind: "none" }
    | { kind: "ahead"; count: number; upstream: string };

  export function outgoingState(
    branch: BranchInfo | null,
    listing: { loading: boolean; error: string | null },
  ): OutgoingState {
    // A branch row already read is the best evidence there is; the listing's
    // own state only matters when there is no row to read.
    if (!branch) {
      if (listing.error) return { kind: "unknown", reason: "The branch list could not be read." };
      if (listing.loading) return { kind: "unknown", reason: "Still reading the branch list." };
      return { kind: "unknown", reason: "No branch is checked out." };
    }
    const upstream = branch.upstream?.trim() ?? "";
    if (!upstream) {
      return {
        kind: "unknown",
        reason: "This branch has no upstream, so unpushed commits can't be counted. A review compares against the default branch.",
      };
    }
    if (branch.is_gone) {
      return { kind: "unknown", reason: `The upstream ${upstream} no longer exists, so unpushed commits can't be counted.` };
    }
    return branch.ahead_count > 0
      ? { kind: "ahead", count: branch.ahead_count, upstream }
      : { kind: "none" };
  }
</script>

<script lang="ts">
  import { AlertTriangle, CheckCircle2, ScanSearch } from "@lucide/svelte";
  import { invoke } from "../ipc/invoke";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { summarizeCommitReview, type CommitReviewReport } from "../ops/model";
  import Skeleton from "./Skeleton.svelte";

  /**
   * The local pre-push check, run on demand: conventional format, duplicate
   * subjects, issue links and length. It reads only the checkout's own history,
   * so it works whether or not `gh` is installed or signed in; the Remote pane
   * therefore renders it outside its GitHub availability branch.
   */
  const branchesStore = repoStore.branches;
  const current = $derived($branchesStore.find((b) => b.is_current && !b.is_remote) ?? null);
  const outgoing = $derived(
    outgoingState(current, { loading: $repoStore.isLoading, error: $repoStore.error }),
  );

  let review = $state<CommitReviewReport | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  /** Repository the shown review belongs to; a switch discards it. */
  let reviewedRepo: string | null = null;

  $effect(() => {
    const repo = $currentRepoPath;
    if (repo !== reviewedRepo) {
      reviewedRepo = repo;
      review = null;
      error = null;
      busy = false;
    }
  });

  async function reviewCommits() {
    const repo = $repoStore.currentPath;
    if (!repo || busy) return;
    busy = true;
    error = null;
    try {
      const next = await invoke<CommitReviewReport>("cmd_review_outgoing_commits", { repoPath: repo });
      if ($repoStore.currentPath === repo) review = next;
    } catch (cause) {
      if ($repoStore.currentPath === repo) {
        const copy = describeFailure({ verb: "review", object: "outgoing commits" }, cause);
        error = copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
      }
    } finally {
      if ($repoStore.currentPath === repo) busy = false;
    }
  }
</script>

{#if outgoing.kind !== "none"}
  <section class="gp-card mb-3 max-w-4xl p-4" aria-label="Outgoing commits" data-testid="outgoing-commits">
    <div class="mb-3 flex items-start justify-between gap-3">
      <div class="min-w-0">
        <h3 class="font-semibold">Outgoing commits</h3>
        <p class="text-textMuted">Conventional format, duplicate subjects, issue links and length, checked before you push.</p>
      </div>
      <div class="flex shrink-0 items-center gap-2">
        {#if outgoing.kind === "ahead"}
          <span class="gp-chip" data-tone="neutral" data-kind="label" title="Ahead of {outgoing.upstream}">
            {outgoing.count} unpushed
          </span>
        {/if}
        {#if review}
          <span class="gp-chip" data-tone={review.findings.length === 0 ? "ok" : "warn"} data-kind="label">
            {review.findings.length} {review.findings.length === 1 ? "finding" : "findings"}
          </span>
        {/if}
        <button type="button" class="gp-btn" data-size="sm" onclick={reviewCommits} disabled={busy}>
          <ScanSearch size={13} /> Review messages
        </button>
      </div>
    </div>
    {#if outgoing.kind === "unknown"}
      <div class="gp-notice mb-2" data-tone="unknown" role="status">
        <AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
        <span class="min-w-0 flex-1">Couldn't check for unpushed commits. {outgoing.reason}</span>
      </div>
    {/if}
    {#if error}
      <div class="gp-notice mb-2" data-tone="danger" role="alert">{error}</div>
    {/if}
    {#if busy}
      <div class="py-3" aria-label="Reviewing commit headers"><Skeleton variant="text" count={3} /></div>
    {:else if review}
      <p class="mb-2 text-textSecondary">{summarizeCommitReview(review)} <span class="font-mono text-textMuted">{review.range}</span></p>
      <div class="max-h-56 space-y-1 overflow-auto">
        {#each review.findings as finding, i (`${finding.commit_id}:${finding.code}#${i}`)}
          <div class="rounded-lg bg-surface/50 px-2.5 py-2">
            <div class="flex items-center gap-2">
              <span class="font-mono text-accent">{finding.short_id}</span>
              <span class={finding.severity === "error" ? "text-danger" : finding.severity === "warning" ? "text-warn" : "text-textMuted"}>{finding.code}</span>
            </div>
            <div class="mt-0.5 truncate">{finding.subject}</div>
            <div class="text-textMuted">{finding.detail}</div>
          </div>
        {:else}
          <div class="flex items-center gap-2 py-3 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> No commit-message findings.</div>
        {/each}
      </div>
    {/if}
  </section>
{/if}
