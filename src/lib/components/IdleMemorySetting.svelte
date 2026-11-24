<script lang="ts">
  /**
   * Idle memory management mode (`idle_memory_mode`), Balanced by default.
   *
   * Governs progressive Stage 1 shedding (releasing off-screen GPU terminal
   * contexts, inactive repository graphs, listings, and hidden surfaces) and
   * Stage 2 WebView hibernation while keeping terminal shells and agents alive
   * inside `gitpulse-ptyd`.
   */
  import SettingSegment from "./SettingSegment.svelte";
  import { explainError } from "../workbench/client";
  import { loadOnce } from "../settings/loadOnce";
  import type { IdleMemoryMode } from "../runtime/idleShedder";
  import {
    idleMemoryMode,
    loadIdleMemoryMode,
    loadIdleMemoryStatus,
    saveIdleMemoryMode,
    type IdleMemoryStatus,
  } from "../runtime/idleShedderWiring";

  let { active = true }: { active?: boolean } = $props();

  const IDLE_MEMORY_OPTIONS: readonly { value: IdleMemoryMode; label: string; title: string }[] = [
    {
      value: "off",
      label: "Off",
      title: "Keep caches, GPU terminal renderers, and the main window resident at all times",
    },
    {
      value: "balanced",
      label: "Balanced",
      title: "Shed off-screen caches after 45 s unfocused, deep-shed after 30 s hidden, and hibernate after 10 minutes hidden (Default)",
    },
    {
      value: "aggressive",
      label: "Aggressive",
      title: "Shed off-screen caches after 20 s unfocused, deep-shed after 5 s hidden, and hibernate after 3 minutes hidden or under critical pressure",
    },
  ];

  let busy = $state(false);
  let error = $state("");
  let readOk = $state(false);
  let status = $state<IdleMemoryStatus | null>(null);

  async function load() {
    busy = true;
    error = "";
    try {
      const [_, liveStatus] = await Promise.all([
        loadIdleMemoryMode(),
        loadIdleMemoryStatus().catch(() => null),
      ]);
      status = liveStatus;
      readOk = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function choose(next: IdleMemoryMode) {
    if (busy || next === $idleMemoryMode) return;
    busy = true;
    error = "";
    try {
      await saveIdleMemoryMode(next);
      status = await loadIdleMemoryStatus().catch(() => status);
      readOk = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  const loader = loadOnce(load);
  $effect(() => {
    if (active) loader.request();
  });
</script>

<div class="space-y-1.5 text-meta" data-testid="idle-memory-settings">
  <h3 class="gp-label mb-1">Idle memory efficiency</h3>
  <SettingSegment
    ariaLabel="Idle memory mode"
    options={IDLE_MEMORY_OPTIONS}
    value={$idleMemoryMode}
    disabled={busy}
    onselect={(next) => {
      if (!busy && (readOk || !loader.attempted)) void choose(next);
      else if (!busy) void choose(next);
    }}
  />
  <p class="text-textMuted text-micro leading-snug" data-testid="idle-memory-note">
    Balanced (default) frees off-screen GPU contexts and repository caches when unfocused or hidden
    (Stage 1), then hibernates the idle window after 10 minutes while keeping shells and agent
    sessions alive in <code>gitpulse-ptyd</code> (Stage 2). Off keeps the window resident;
    Aggressive sheds faster and hibernates under critical memory pressure.
  </p>
  {#if status && (status.hibernate_count ?? 0) > 0}
    <p class="text-textMuted text-micro leading-snug" data-testid="idle-memory-status">
      Hibernated {status.hibernate_count} {status.hibernate_count === 1 ? "time" : "times"} this run{#if typeof status.last_wake_ms === "number"} · last wake {status.last_wake_ms} ms{/if}.
    </p>
  {/if}
  {#if error}
    <p class="text-warn text-micro" data-testid="idle-memory-error">{error}</p>
    <button
      type="button"
      class="gp-btn text-meta px-2 py-0.5"
      disabled={busy}
      onclick={() => loader.retry()}
    >
      Try again
    </button>
  {/if}
</div>
