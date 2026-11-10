<script lang="ts">
  /** The verdict on a board card: a small chip, drawn only for a task that has been verified. */
  import { VERDICT_HINTS, VERDICT_LABELS, type CardVerdict } from "../workbench/verification";

  let { verdict }: { verdict: CardVerdict } = $props();

  /** Tone says what the state means. "Couldn't run" is unknown (dashed), never a pass or a failure. */
  const TONES: Record<CardVerdict["state"], string> = {
    passed: "ok",
    failed: "danger",
    degraded: "warn",
    could_not_run: "unknown",
    running: "neutral",
  };
</script>

<span class="gp-chip w-fit font-semibold {verdict.state === 'could_not_run' ? 'border-dashed' : ''}" data-tone={TONES[verdict.state]} data-testid="card-verdict" data-state={verdict.state} data-stale={verdict.stale || undefined} title={VERDICT_HINTS[verdict.state]}>{VERDICT_LABELS[verdict.state]}{#if verdict.stale}<span class="sr-only"> (for an earlier commit)</span><span aria-hidden="true"> *</span>{/if}</span>
