<script lang="ts">
  /**
   * The host-load sensor's kill switch (`host_load.enabled`), on by default.
   *
   * Off stops the readings, not the heartbeat: the sampler keeps publishing
   * a snapshot that says "switched off", so other GitPulse processes can tell
   * that apart from a sampler that died. Health shows what the sensor reads.
   */
  import { explainError } from "../workbench/client";
  import SettingToggle from "./SettingToggle.svelte";
  import {
    DEFAULT_HOST_LOAD_SETTINGS,
    loadHostLoadSettings,
    saveHostLoadSettings,
    type HostLoadSettings,
  } from "../stores/hostLoadStore";
  import { loadOnce } from "../settings/loadOnce";

  let { active = true }: { active?: boolean } = $props();

  let settings = $state<HostLoadSettings>(DEFAULT_HOST_LOAD_SETTINGS);
  let busy = $state(false);
  let error = $state("");
  // False until a read succeeds: before that `settings` is the defaults, and a
  // toggle on defaults would save them over the real configuration.
  let readOk = $state(false);

  async function load() {
    busy = true;
    error = "";
    try {
      settings = await loadHostLoadSettings();
      readOk = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function save(change: Partial<HostLoadSettings>) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      settings = await saveHostLoadSettings({ ...settings, ...change });
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  // One automatic attempt: a failed load shows its error and a Retry button
  // rather than re-running this effect as `busy` settles.
  const loader = loadOnce(load);
  $effect(() => {
    if (active) loader.request();
  });
</script>

<div class="space-y-1 text-meta" data-testid="host-load-settings">
  <div class="text-textMuted text-micro mb-1.5">Host load</div>
  <SettingToggle
    label="Measure memory pressure and agent sessions"
    description="On by default. Every 2 seconds GitPulse reads this computer's memory pressure, swap and compressor, and finds the agent sessions running on it, for Health and for the agents GitPulse launches. Off stops the readings; nothing is ever sent anywhere."
    checked={settings.enabled}
    disabled={busy || !readOk}
    onchange={(next) => void save({ enabled: next })}
  />
  <SettingToggle
    label="Refuse agent launches under severe memory pressure"
    description="On by default. When the kernel reports critical memory pressure, or the memory compressor is thrashing, GitPulse refuses to start an agent until it clears, and says which. You can always choose Admit anyway in Health. Off still sheds GitPulse's own background and refresh work under pressure, but never refuses a launch. Needs the measurement above."
    checked={settings.govern_launches}
    disabled={busy || !readOk || !settings.enabled}
    onchange={(next) => void save({ govern_launches: next })}
  />
  {#if error}
    <p class="text-warn">{error}</p>
    <button type="button" class="gp-btn text-meta px-2 py-0.5" disabled={busy} onclick={() => loader.retry()}>Retry</button>
  {/if}
</div>
