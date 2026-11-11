<script lang="ts">
  /**
   * The verdict on a task, with each criterion's result one fold away.
   * Presentational: the state arrives as a prop and nothing here reads the host.
   */
  import { VERDICT_HINTS, VERDICT_LABELS, type Verification } from "../workbench/verification";

  let { verification }: { verification: Verification } = $props();
  const state = $derived(verification.state);
  const record = $derived(verification.record);
  const unmet = $derived(record?.criteria.filter((c) => c.status !== "met").length ?? 0);
  /** Tone says what the state means. "Couldn't run" is unknown (dashed), never a pass or a failure. */
  const TONES: Record<string, string> = {
    passed: "ok",
    failed: "danger",
    degraded: "warn",
    could_not_run: "unknown",
  };
  const tone = $derived(TONES[state] ?? "neutral");
</script>

<div class="verdict" data-testid="verification" data-state={state}>
  <span class="gp-chip font-semibold {state === 'could_not_run' ? 'border-dashed' : ''}" data-tone={tone} data-testid="verification-chip" data-state={state}>{VERDICT_LABELS[state]}</span>
  {#if verification.stale && state !== "none"}
    <span class="text-xs text-textMuted" data-testid="verification-stale">for an earlier commit</span>
  {/if}
  <span class="text-xs text-textMuted">{VERDICT_HINTS[state]}</span>
  {#if record && (record.criteria.length || record.degraded.length)}
    <details class="detail" data-testid="verification-detail">
      <summary>{record.criteria.length} criteri{record.criteria.length === 1 ? "on" : "a"}{unmet ? `, ${unmet} not met` : ""}</summary>
      <ul class="criteria">
        {#each record.criteria as c (c.index)}
          <li data-status={c.status}>
            <span class="text-meta font-semibold">{c.status}</span> {c.text}
            {#if c.findings.length}
              <ul class="findings">{#each c.findings as finding, i (i)}<li>{finding}</li>{/each}</ul>
            {/if}
          </li>
        {/each}
      </ul>
      {#if record.degraded.length}
        <p class="unchecked">Not checked</p>
        <ul class="findings">{#each record.degraded as item, i (i)}<li>{item}</li>{/each}</ul>
      {/if}
    </details>
  {/if}
</div>

<style>
  .verdict{display:flex;flex-wrap:wrap;align-items:center;gap:.5rem}
  .detail{flex-basis:100%;font-size:var(--text-xs)}
  .criteria,.findings{margin:.25rem 0 0 1rem;padding:0}
  .unchecked{margin:.4rem 0 0;font-weight:600}
</style>
