<script lang="ts">
  import { onDestroy } from "svelte";
  import { explainError, type TaskRun } from "../workbench/client";
  import { readAttemptReview, recordAttemptReview, reviewSummary, setReviewGate, type AttemptReview, type ReviewDecision } from "../workbench/attemptReview";
  let { run }: { run: TaskRun } = $props();
  let view = $state<AttemptReview | null>(null), note = $state(""), busy = $state(false), error = $state("");
  let disposed = false;
  async function step(body: () => Promise<AttemptReview | void>) {
    if (busy) return;
    busy = true; error = "";
    try { const next = await body(); if (!disposed && next) view = next; }
    catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) busy = false; }
  }
  const load = () => step(() => readAttemptReview(run.id));
  function decide(decision: ReviewDecision) {
    return step(async () => { const next = await recordAttemptReview(run.id, decision, note); note = ""; return next; });
  }
  function toggle(enabled: boolean) {
    return step(async () => { await setReviewGate(run.cwd, enabled); return readAttemptReview(run.id); });
  }
  const undecided = $derived(view?.ended === true && (view.review.status === "unreviewed" || view.review.status === "stale"));
  onDestroy(() => { disposed = true; });
</script>

<section class="attempt-review" aria-label="Merge review" data-testid="attempt-review">
  {#if !view}
    <button class="gp-btn" type="button" onclick={load} disabled={busy} title="Shows whether this attempt's commits were reviewed, and whether this repository requires that before a merge.">Merge review</button>
  {:else}
    <p>{reviewSummary(view)}</p>
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
    <button class="gp-btn" type="button" onclick={load} disabled={busy}>Refresh review</button>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
</section>
