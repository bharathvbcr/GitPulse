<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { explainError } from "../workbench/client";
  import { beginWave, currentWave, resumeActiveWave } from "../workbench/waveHost";
  import { DEFAULT_WAVES, DEFAULT_WAVE_SIZE, MAX_WAVES, MAX_WAVE_SIZE, clampWaveSize, clampWaves, type MemberPhase, type WaveRecord, type WaveRunner } from "../workbench/waveLaunch";

  /**
   * Run the board in waves: launch the top N suggested tasks, wait for each to
   * be verified, merge the passing ones one at a time, then launch the next N.
   * It only drives `waveHost`; the wave itself outlives this panel (closing
   * it does not stop it) and survives a restart on the cards it is working on.
   */
  interface Props { repositoryId: string | null }
  let { repositoryId }: Props = $props();

  let size = $state(DEFAULT_WAVE_SIZE);
  let waves = $state(DEFAULT_WAVES);
  let runner = $state<WaveRunner | null>(currentWave());
  let record = $state<WaveRecord | null>(currentWave()?.record() ?? null);
  let error = $state("");
  let partial = $state(false);
  let busy = $state(false);
  let stopPoll: (() => void) | null = null;
  let disposed = false;

  const running = $derived(record?.state === "running");
  const LABELS: Record<MemberPhase, string> = {
    launching: "Launching", running: "Agent working", verifying: "Verifying", ready: "Verified, waiting to merge",
    merging: "Merging", merged: "Merged", held: "Held for review", stopped: "Stopped",
  };
  const STATES: Record<WaveRecord["state"], string> = {
    running: "Running", done: "Finished", cancelled: "Stopped", halted: "Halted after a failed re-verify", failed: "Stopped: its state could not be kept",
  };

  function watch(next: WaveRunner) {
    runner = next; record = next.record();
    stopPoll?.();
    // Stops while the window is hidden and catches up when it returns.
    stopPoll = createAdaptiveTimer(() => {
      if (!runner || disposed) return;
      record = runner.record();
      if (record.state !== "running") { stopPoll?.(); stopPoll = null; }
    }, 1000);
    void next.finished.then((done) => { if (!disposed && runner === next) record = done; });
  }

  onMount(async () => {
    if (runner) { watch(runner); return; }
    try {
      const found = await resumeActiveWave();
      if (disposed) return;
      partial = !found.complete;
      if (found.runner) watch(found.runner);
    } catch (cause) { if (!disposed) error = `A wave left running could not be resumed: ${explainError(cause)}`; }
  });
  onDestroy(() => { disposed = true; stopPoll?.(); });

  async function start() {
    if (!repositoryId || busy || running) return;
    busy = true; error = "";
    try { watch(await beginWave({ repositoryId, n: clampWaveSize(size), waves: clampWaves(waves) })); }
    catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) busy = false; }
  }
  async function stop() {
    if (!runner || busy) return;
    busy = true;
    try { await runner.cancel(); if (!disposed && runner) record = runner.record(); }
    catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) busy = false; }
  }
</script>

<section class="wave" data-testid="wave-panel" aria-label="Run the board in waves">
  <div class="head">
    <h2>Run in waves</h2>
    <label>Tasks per wave
      <input class="gp-field" type="number" min="1" max={MAX_WAVE_SIZE} bind:value={size} disabled={running || busy} data-testid="wave-size" />
    </label>
    <label>Waves
      <input class="gp-field" type="number" min="1" max={MAX_WAVES} bind:value={waves} disabled={running || busy} data-testid="wave-count" />
    </label>
    {#if running}
      <button type="button" class="gp-btn" onclick={() => void stop()} disabled={busy} data-testid="wave-stop">Stop</button>
    {:else}
      <button type="button" class="gp-btn-primary" onclick={() => void start()} disabled={busy || !repositoryId} title={repositoryId ? "Launch the top suggested tasks, verify, merge the passing ones, then the next." : "Choose a repository board: a wave launches in one repository."} data-testid="wave-start">{busy ? "Starting…" : "Start"}</button>
    {/if}
  </div>
  {#if error}<p class="note" role="alert" data-testid="wave-error">{error}</p>{/if}
  {#if partial}<p class="note" role="status" data-testid="wave-partial">Not every task was read while looking for a wave left running, so none being shown is not proof there is none.</p>{/if}
  {#if record}
    <p class="note" role="status" data-testid="wave-state">{STATES[record.state]}: batch {record.batch}{record.batchesLeft > 0 && running ? `, ${record.batchesLeft} more after it` : ""}. Nothing is pushed.{record.note ? ` ${record.note}` : ""}</p>
    <ul class="members">
      {#each record.members as member (member.taskId)}
        <li data-testid="wave-member" data-phase={member.phase}>
          <span class="title">{member.title}</span>
          <span class="phase">{LABELS[member.phase]}</span>
          {#if member.detail}<span class="detail">{member.detail}</span>{/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .wave{margin:8px 14px 0;padding:10px 12px;border-radius:12px;border:1px solid rgb(var(--c-border) / 0.65);display:flex;flex-direction:column;gap:8px}
  .head{display:flex;align-items:center;gap:8px;flex-wrap:wrap}
  h2{font-size:12px;font-weight:650;margin:0;flex:1}
  label{font-size:11px;display:flex;align-items:center;gap:6px}
  input{width:52px}
  .note{font-size:11px;margin:0;color:rgb(var(--c-text-muted))}
  .members{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:4px}
  .members li{display:flex;flex-wrap:wrap;gap:4px 8px;align-items:baseline;font-size:12px}
  .title{font-weight:550;overflow-wrap:anywhere}
  .phase{font-size:10px;padding:1px 5px;border-radius:4px;background:color-mix(in srgb,rgb(var(--c-accent)) 9%,transparent)}
  li[data-phase="held"] .phase{background:rgb(209 90 100 / .16);color:#d15a64}
  .detail{font-size:11px;color:rgb(var(--c-text-muted));overflow-wrap:anywhere}
</style>
