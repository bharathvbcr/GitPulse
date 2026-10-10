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
</script>

<div class="verdict" data-testid="verification" data-state={state}>
  <span class="chip" data-testid="verification-chip" data-state={state}>{VERDICT_LABELS[state]}</span>
  {#if verification.stale && state !== "none"}
    <span class="stale" data-testid="verification-stale">for an earlier commit</span>
  {/if}
  <span class="hint">{VERDICT_HINTS[state]}</span>
  {#if record && (record.criteria.length || record.degraded.length)}
    <details class="detail" data-testid="verification-detail">
      <summary>{record.criteria.length} criteri{record.criteria.length === 1 ? "on" : "a"}{unmet ? `, ${unmet} not met` : ""}</summary>
      <ul class="criteria">
        {#each record.criteria as c (c.index)}
          <li data-status={c.status}>
            <span class="status">{c.status}</span> {c.text}
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
  .chip{padding:.1rem .55rem;border-radius:999px;font-size:.75rem;font-weight:600;border:1px solid currentColor}
  .chip[data-state="passed"]{color:var(--c-status-green)}
  .chip[data-state="failed"]{color:var(--c-status-red)}
  .chip[data-state="degraded"]{color:var(--c-status-amber)}
  .chip[data-state="could_not_run"]{color:var(--c-status-amber);border-style:dashed}
  .chip[data-state="running"],.chip[data-state="none"]{color:var(--c-text-muted)}
  .hint,.stale{font-size:.75rem;opacity:.8}
  .detail{flex-basis:100%;font-size:.8rem}
  .criteria,.findings{margin:.25rem 0 0 1rem;padding:0}
  .status{font-weight:600;text-transform:uppercase;font-size:.65rem}
  .unchecked{margin:.4rem 0 0;font-weight:600}
</style>
