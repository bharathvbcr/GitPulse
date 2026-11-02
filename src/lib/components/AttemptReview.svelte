<script lang="ts">
  import { onDestroy } from "svelte";
  import { explainError, type TaskRun } from "../workbench/client";
  import { readAttemptReview, recordAttemptReview, reviewSummary, setReviewGate, type AttemptReview, type ReviewDecision } from "../workbench/attemptReview";
  import { attemptDirtyFiles, mergeAttemptWorktree, openAttemptDiff, reviewAttemptUncommitted, type AttemptDiffOpening } from "../workbench/attemptWorktree";
  import { plural } from "../format";
  let { run }: { run: TaskRun } = $props();
  let view = $state<AttemptReview | null>(null), note = $state(""), busy = $state(false), error = $state("");
  /** Files the checkout holds uncommitted: null when they could not be counted, which is not "none". */
  let dirty = $state<number | null>(null);
  let opening = $state<AttemptDiffOpening | null>(null);
  let landed = $state("");
  let disposed = false;
  async function step(body: () => Promise<AttemptReview | void>) {
    if (busy) return;
    busy = true; error = "";
    try { const next = await body(); if (!disposed && next) view = next; }
    catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) busy = false; }
  }
  async function read(): Promise<AttemptReview> {
    const next = await readAttemptReview(run.id);
    // Counted with the review so "Uncommitted files" is offered for what is there; a count that fails is unknown, not zero.
    const count = await attemptDirtyFiles(run).catch(() => null);
    if (!disposed) dirty = count;
    return next;
  }
  const load = () => step(read);
  function decide(decision: ReviewDecision) {
    return step(async () => { const next = await recordAttemptReview(run.id, decision, note); note = ""; return next; });
  }
  function toggle(enabled: boolean) {
    return step(async () => { await setReviewGate(run.cwd, enabled); return readAttemptReview(run.id); });
  }
  function viewDiff() {
    const current = view;
    if (!current) return;
    return step(async () => { const result = await openAttemptDiff(run, current); if (!disposed) opening = result; });
  }
  const viewUncommitted = () => step(() => reviewAttemptUncommitted(run));
  /** Lands exactly the commit that was approved, so approval and landing bind the same one. */
  function land() {
    const current = view;
    if (!current) return;
    return step(async () => {
      const result = await mergeAttemptWorktree(run, { expectedTip: current.head_oid });
      if (result && !disposed) landed = result.message;
    });
  }
  const undecided = $derived(view?.ended === true && (view.review.status === "unreviewed" || view.review.status === "stale"));
  const hasCommits = $derived(view !== null && view.head_oid !== view.base_oid);
  const landable = $derived(view?.ended === true && view.review.status === "approved");
  // A notice belongs to the range it was made for; a refresh that moved the head retires it.
  const notices = $derived(view !== null && opening !== null && opening.to === view.head_oid && opening.from === view.base_oid ? opening.notices : []);
  onDestroy(() => { disposed = true; });
</script>

<section class="attempt-review" aria-label="Merge review" data-testid="attempt-review">
  {#if landed}
    <!-- The worktree is gone: nothing else here can act on it. -->
    <p role="status" data-testid="attempt-landed">{landed}</p>
  {:else if !view}
    <button class="gp-btn" type="button" onclick={load} disabled={busy} title="Shows whether this attempt's commits were reviewed, and whether this repository requires that before a merge.">Merge review</button>
  {:else}
    <p>{reviewSummary(view)}</p>
    <div class="actions">
      {#if hasCommits}
        <button class="gp-btn" type="button" onclick={viewDiff} disabled={busy} data-testid="attempt-view-diff" title="Opens what this attempt committed, against the commit it started from, in the repository view.">View diff ({plural(view.files_changed, "file")})</button>
      {:else}
        <span data-testid="attempt-no-commits">This attempt has not committed anything.</span>
      {/if}
      {#if dirty === null || dirty > 0}
        <button class="gp-btn" type="button" onclick={viewUncommitted} disabled={busy} data-testid="attempt-view-uncommitted" title={dirty === null ? "The uncommitted files could not be counted." : "Opens the files this attempt left uncommitted. The review does not cover them."}>Uncommitted files{dirty === null ? "" : ` (${dirty})`}</button>
      {/if}
    </div>
    {#each notices as notice}<p role="alert" data-testid="attempt-diff-notice">{notice}</p>{/each}
    <label>
      <input type="checkbox" checked={view.gate_on === true} disabled={busy || view.gate_on === null} onchange={(event) => toggle(event.currentTarget.checked)} />
      Require a review before agent attempts are merged in this repository
    </label>
    {#if view.gate_error}<p role="alert">Whether reviews are required could not be read, so attempt merges are refused: {view.gate_error}</p>{/if}
    {#if undecided}
      <textarea class="gp-field gp-field-multi" rows="2" aria-label="Review note" placeholder="Note (required to merge without review)" bind:value={note}></textarea>
      <div class="actions">
        <button class="gp-btn-primary" type="button" onclick={() => decide("approve")} disabled={busy} title="Approves exactly this commit range. A later commit needs a new review.">Approve for merge</button>
        <button class="gp-btn" type="button" onclick={() => decide("request_changes")} disabled={busy}>Request changes</button>
        <button class="gp-btn" type="button" onclick={() => decide("deny")} disabled={busy}>Deny</button>
        <button class="gp-btn" type="button" onclick={() => decide("merge_unreviewed")} disabled={busy || !note.trim()} title="Records that this exact range is merged without review, and why.">Merge without review</button>
      </div>
    {/if}
    {#if landable}
      <div class="actions">
        <button class="gp-btn-primary" type="button" onclick={land} disabled={busy} data-testid="attempt-land" title="Merges or squashes exactly the approved commit into the main checkout's branch, then removes this worktree and its branch.">Land and clean up</button>
      </div>
    {/if}
    <button class="gp-btn" type="button" onclick={load} disabled={busy}>Refresh review</button>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
</section>
