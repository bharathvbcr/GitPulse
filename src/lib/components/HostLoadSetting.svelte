<script lang="ts">
  /**
   * The host-load sensor's kill switch (`host_load.enabled`), on by default,
   * the launch-refusal switch, and a reset for the per-input thresholds below
   * them (each input is its own row, `HostLoadInputSetting`).
   *
   * Off stops the readings, not the heartbeat: the sampler keeps publishing
   * a snapshot that says "switched off", so other GitPulse processes can tell
   * that apart from a sampler that died. Health shows what the sensor reads.
   */
  import { explainError } from "../workbench/client";
  import SettingToggle from "./SettingToggle.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import {
    DEFAULT_HOST_LOAD_SETTINGS,
    DEFAULT_THRESHOLDS,
    hostLoadSettings,
    loadHostLoadSettings,
    updateHostLoadSettings,
    type HostLoadHookMode,
    type HostLoadSettings,
  } from "../stores/hostLoadStore";
  import { loadOnce } from "../settings/loadOnce";

  let { active = true }: { active?: boolean } = $props();

  // The shared copy: the input rows save through the same store, so a toggle
  // here never saves their thresholds back stale.
  const settings = $derived<HostLoadSettings>($hostLoadSettings ?? DEFAULT_HOST_LOAD_SETTINGS);
  const thresholdsAreDefault = $derived(JSON.stringify(settings.thresholds) === JSON.stringify(DEFAULT_THRESHOLDS));
  const HOOK_MODES: readonly { value: HostLoadHookMode; label: string; title: string }[] = [
    { value: "enforce", label: "Refuse", title: "Refuse a heavy command the host cannot take, with a reason" },
    { value: "advise", label: "Advise", title: "Tell the agent, never refuse" },
    { value: "off", label: "Off", title: "Say nothing about load" },
  ];
  let busy = $state(false);
  let error = $state("");
  // False until a read succeeds: before that `settings` is the defaults, and a
  // toggle on defaults would save them over the real configuration.
  let readOk = $state(false);

  async function load() {
    busy = true;
    error = "";
    try {
      await loadHostLoadSettings();
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
      await updateHostLoadSettings(change);
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
  <h3 class="gp-label mb-1.5">Host load</h3>
  <SettingToggle
    label="Measure memory pressure and agent sessions"
    description="On by default. Every 2 seconds GitPulse reads this computer's memory pressure, swap and compressor, and finds the agent sessions running on it, for Health and for the agents GitPulse launches. Off stops the readings; nothing is ever sent anywhere."
    checked={settings.enabled}
    disabled={busy || !readOk}
    onchange={(next) => void save({ enabled: next })}
  />
  <SettingToggle
    label="Refuse agent launches when the machine is overloaded"
    description="On by default. When the kernel reports critical memory pressure, the memory compressor is thrashing, or an input below is set to refuse launches and reaches its limit, GitPulse refuses to start an agent until it clears, and says which. You can always choose Admit anyway in Health. Off still slows GitPulse's own background and refresh work under load, but never refuses a launch. Needs the measurement above."
    checked={settings.govern_launches}
    disabled={busy || !readOk || !settings.enabled}
    onchange={(next) => void save({ govern_launches: next })}
  />
  <div class="space-y-1 pt-1" data-testid="host-load-hook-mode">
    <p class="text-text">Command gate: heavy commands in agent sessions</p>
    <p class="text-textMuted text-micro leading-snug">
      Refuse (the default): when the machine is overloaded, GitPulse's hook refuses a build or test command (cargo, go, npm test, vitest) in any Claude session with a reason telling the agent to wait, even a session GitPulse did not launch. Advise: the same message, but the command runs. Off: the hook says nothing about load. A command it couldn't check (GitPulse not running, or too old) is never refused; the agent is told it was not checked.
    </p>
    <SettingSegment
      ariaLabel="What the command gate does with a heavy command under load"
      options={HOOK_MODES}
      value={settings.hook_mode}
      onselect={(next) => {
        // Before a read has succeeded `settings` is the defaults; saving one
        // would write them over the real configuration.
        if (readOk && !busy) void save({ hook_mode: next });
      }}
    />
  </div>
  <div class="flex items-center gap-2 pt-1">
    <p class="text-textMuted text-micro leading-snug flex-1">
      Each input below can slow GitPulse's background work, also refuse agent launches, or be ignored. The starting values come from one crash and are a protective guess, not a measured optimum.
    </p>
    <button
      type="button"
      class="gp-btn text-meta px-2 py-0.5 shrink-0"
      disabled={!readOk || busy || thresholdsAreDefault}
      data-testid="host-load-reset-thresholds"
      onclick={() => void save({ thresholds: DEFAULT_THRESHOLDS })}
    >
      Reset to defaults
    </button>
  </div>
  {#if error}
    <p class="text-warn">{error}</p>
    <button type="button" class="gp-btn text-meta px-2 py-0.5" disabled={busy} onclick={() => loader.retry()}>Try again</button>
  {/if}
</div>
