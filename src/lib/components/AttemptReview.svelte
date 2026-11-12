<script lang="ts">
  import { onDestroy } from "svelte";
  import type { TaskRun } from "../workbench/client";
  import { describeFailure } from "../ui/copy/describeFailure";
  import Toggle from "./Toggle.svelte";
  import { readAttemptReview, recordAttemptReview, reviewSummary, setReviewGate, type AttemptReview, type ReviewDecision } from "../workbench/attemptReview";
  import { attemptDirtyFiles, mergeAttemptWorktree, openAttemptDiff, reviewAttemptUncommitted, type AttemptDiffOpening } from "../workbench/attemptWorktree";
  import { plural } from "../format";
  import { diagnostics, formatDiagnosticFailure } from "../diagnostics/diagnostics";
  /** `onlanded` is told once the worktree is gone, so the page around this can stop offering it. */
  let { run, onlanded }: { run: TaskRun; onlanded?: () => void } = $props();
  let view = $state<AttemptReview | null>(null), note = $state(""), busy = $state(false), error = $state("");
  /** Files the checkout holds uncommitted: null when they could not be counted, which is not "none". */
  let dirty = $state<number | null>(null);
  let opening = $state<AttemptDiffOpening | null>(null);
  let landed = $state("");
  let disposed = false;
  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }
  async function step(body: () => Promise<AttemptReview | void>, action: { verb: string; object: string } = { verb: "update", object: "the merge review" }) {
    if (busy) return;
    busy = true; error = "";
    try { const next = await body(); if (!disposed && next) view = next; }
    catch (cause) { if (!disposed) error = failure(action.verb, action.object, cause); }
    finally { if (!disposed) busy = false; }
  }
  async function read(): Promise<AttemptReview> {
    const next = await readAttemptReview(run.id);
    // Counted with the review so "Uncommitted files" is offered for what is there; a count that fails is unknown, not zero.
    const count = await attemptDirtyFiles(run).catch((cause: unknown) => {
      diagnostics.warn("attempt-review", `Uncommitted files of ${run.cwd} couldn't be counted: ${formatDiagnosticFailure(cause)}`);
      return null;
    });
    if (!disposed) dirty = count;
    return next;
  }
  const load = () => step(read, { verb: "read", object: "the merge review" });
  function decide(decision: ReviewDecision) {
    return step(async () => { const next = await recordAttemptReview(run.id, decision, note); note = ""; return next; }, { verb: "record", object: "the review decision" });
  }
  function toggle(enabled: boolean) {
    return step(async () => { await setReviewGate(run.cwd, enabled); return readAttemptReview(run.id); }, { verb: "change", object: "the review requirement" });
  }
  function viewDiff() {
    const current = view;
    if (!current) return;
    return step(async () => { const result = await openAttemptDiff(run, current); if (!disposed) opening = result; }, { verb: "open", object: "the diff" });
  }
  const viewUncommitted = () => step(() => reviewAttemptUncommitted(run), { verb: "open", object: "the uncommitted files" });
  /** Lands exactly the commit that was approved, so approval and landing bind the same one. */
  function land() {
    const current = view;
    if (!current) return;
    return step(async () => {
      const result = await mergeAttemptWorktree(run, { expectedTip: current.head_oid });
      if (!result) return;
      // Told even when this view is gone: the worktree is removed either way.
      // A merge whose removal did not complete leaves it, and says nothing.
      if (result.worktreeRemoved) onlanded?.();
      if (!disposed) landed = result.message;
    }, { verb: "land", object: "the run" });
  }
  const undecided = $derived(view?.ended === true && (view.review.status === "unreviewed" || view.review.status === "stale"));
  const hasCommits = $derived(view !== null && view.head_oid !== view.base_oid);
  const landable = $derived(view?.ended === true && view.review.status === "approved");
  // A notice belongs to the range it was made for; a refresh that moved the head retires it.
  const notices = $derived(view !== null && opening !== null && opening.to === view.head_oid && opening.from === view.base_oid ? opening.notices : []);
  onDestroy(() => { disposed = true; });
</script>

<section class="attempt-review flex flex-col items-start gap-2" aria-label="Merge review" data-testid="attempt-review">
  {#if landed}
    <!-- The worktree is gone: nothing else here can act on it. -->
    <p role="status" data-testid="attempt-landed">{landed}</p>
  {:else if !view}
    <button class="gp-btn" type="button" onclick={load} disabled={busy} title="Shows whether this run's commits were reviewed, and whether this repository requires that before a merge.">Merge review</button>
  {:else}
    <p>{reviewSummary(view)}</p>
    <div class="actions flex flex-wrap items-center gap-2">
      {#if hasCommits}
        <button class="gp-btn" type="button" onclick={viewDiff} disabled={busy} data-testid="attempt-view-diff" title="Opens what this run committed, against the commit it started from, in the repository view.">View diff ({plural(view.files_changed, "file")})</button>
      {:else}
        <span data-testid="attempt-no-commits">This run has not committed anything.</span>
      {/if}
      {#if dirty === null || dirty > 0}
        <button class="gp-btn" type="button" onclick={viewUncommitted} disabled={busy} data-testid="attempt-view-uncommitted" title={dirty === null ? "The uncommitted files couldn't be counted." : "Opens the files this run left uncommitted. The review does not cover them."}>Uncommitted files{dirty === null ? "" : ` (${dirty})`}</button>
      {/if}
    </div>
    {#each notices as notice}<div class="gp-notice" data-tone="warn" role="alert" data-testid="attempt-diff-notice">{notice}</div>{/each}
    <Toggle checked={view.gate_on === true} disabled={busy || view.gate_on === null} label="Require a review before agent runs are merged in this repository" onchange={(next) => void toggle(next)} />
    {#if view.gate_error}<div class="gp-notice" data-tone="unknown" role="alert">Couldn't check whether reviews are required, so merges of agent runs are refused. {view.gate_error}</div>{/if}
    {#if undecided}
      <textarea class="gp-field gp-field-multi w-full" rows="2" aria-label="Review note" placeholder="Note (required to merge without review)" bind:value={note}></textarea>
      <div class="actions flex flex-wrap items-center gap-2">
        <button class="gp-btn" data-variant="primary" type="button" onclick={() => decide("approve")} disabled={busy} title="Approves exactly this commit range. A later commit needs a new review.">Approve for merge</button>
        <button class="gp-btn" type="button" onclick={() => decide("request_changes")} disabled={busy}>Request changes</button>
        <button class="gp-btn" type="button" onclick={() => decide("deny")} disabled={busy}>Deny</button>
        <button class="gp-btn" type="button" onclick={() => decide("merge_unreviewed")} disabled={busy || !note.trim()} title="Records that this exact range is merged without review, and why.">Merge without review</button>
      </div>
    {/if}
    {#if landable}
      <div class="actions flex flex-wrap items-center gap-2">
        <button class="gp-btn" data-variant="primary" type="button" onclick={land} disabled={busy} data-testid="attempt-land" title="Merges or squashes exactly the approved commit into the main worktree's branch, then removes this worktree and its branch.">Land and clean up</button>
      </div>
    {/if}
    <button class="gp-btn" type="button" onclick={load} disabled={busy}>Refresh review</button>
  {/if}
  {#if error}<div class="gp-notice" data-tone="danger" role="alert">{error}</div>{/if}
</section>
