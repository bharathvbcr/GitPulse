<script lang="ts">
  /**
   * GitPulse's two Lappi switches, both off by default.
   *
   * Asking lets Lappi pre-select a type only when GitPulse's own classifier
   * could not type the staged change; the user still edits and commits the
   * message. Recording keeps local caller records under Lappi's held-out store
   * and sends nothing anywhere. What recording has done is shown beside the
   * switch, because "on, nothing written" and "on, stopped at the cap" look
   * identical from the switch alone.
   */
  import { explainError } from "../workbench/client";
  import SettingToggle from "./SettingToggle.svelte";
  import { lappi, refreshLappi, saveLappi, type LappiSettings } from "../stores/lappiStore";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let error = $state("");
  let loaded = false;

  const view = $derived($lappi);

  async function load() {
    busy = true;
    error = "";
    try {
      await refreshLappi();
      loaded = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function save(next: LappiSettings) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      await saveLappi(next);
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    if (active && !loaded && !busy) void load();
  });
</script>

<div class="space-y-1 text-[11px]" data-testid="lappi-settings">
  <div class="text-textMuted text-[10px] mb-1.5">Lappi</div>
  <SettingToggle
    label="Ask Lappi on ambiguous commit types"
    description="Off by default. When the staged change has no clear type, ask a running Lappi agent on this computer. Its answer can only pre-select the type in the draft you edit."
    checked={view.settings.ask_on_ambiguous_commit_type}
    disabled={busy || !view.transport_supported}
    onchange={(next) => void save({ ...view.settings, ask_on_ambiguous_commit_type: next })}
  />
  <SettingToggle
    label="Record Lappi caller data (local only)"
    description="Off by default. Keeps counts about ambiguous drafts and the type you committed — never the patch or the message — in Lappi's held-out folder on this computer. Nothing is uploaded."
    checked={view.settings.record_caller_data}
    disabled={busy || !view.transport_supported}
    onchange={(next) => void save({ ...view.settings, record_caller_data: next })}
  />
  {#if view.collect_forced_off}
    <p class="text-textMuted" data-testid="lappi-collect-off">LAPPI_COLLECT=0 is set, so nothing is recorded.</p>
  {/if}
  {#if view.store}
    <p class="text-textMuted" data-testid="lappi-store-status">
      Recorded {view.store.written}, dropped {view.store.dropped}{view.store.stopped
        ? ` · stopped: ${view.store.stopped.replaceAll("_", " ")}`
        : ""}{view.store.last_error ? ` · ${view.store.last_error}` : ""}
    </p>
  {/if}
  {#if error}<p class="text-amber-600 dark:text-amber-400" role="alert">{error}</p>{/if}
</div>
