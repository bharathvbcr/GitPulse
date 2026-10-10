<script lang="ts">
  /** The verdict on a board card: a small chip, drawn only for a task that has been verified. */
  import { VERDICT_HINTS, VERDICT_LABELS, type CardVerdict } from "../workbench/verification";

  let { verdict }: { verdict: CardVerdict } = $props();
</script>

<span class="card-verdict" data-testid="card-verdict" data-state={verdict.state} data-stale={verdict.stale || undefined} title={VERDICT_HINTS[verdict.state]}>{VERDICT_LABELS[verdict.state]}{#if verdict.stale}<span class="sr-only"> (for an earlier commit)</span><span aria-hidden="true"> *</span>{/if}</span>

<style>
  .card-verdict{display:inline-block;width:fit-content;padding:0 .4rem;border-radius:999px;font-size:.65rem;font-weight:600;border:1px solid currentColor}
  .card-verdict[data-state="passed"]{color:var(--c-status-green)}
  .card-verdict[data-state="failed"]{color:var(--c-status-red)}
  .card-verdict[data-state="degraded"]{color:var(--c-status-amber)}
  .card-verdict[data-state="could_not_run"]{color:var(--c-status-amber);border-style:dashed}
  .card-verdict[data-state="running"]{color:var(--c-text-muted)}
</style>
