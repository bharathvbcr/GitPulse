<script lang="ts">
  /**
   * What of DevCouncil is installed, and whether the copy that is installed is
   * the one that answers.
   *
   * `ExternalToolsPanel` above this covers the two binaries GitPulse can
   * install and manage. This covers the rest of the suite the setup wizard
   * offers — `dcstore`, `dcverify`, `dcgrep` and the Go host — which nothing
   * probed before, so an "analysis suite" install reported success from a
   * shell exit code and the app could not say what actually arrived.
   *
   * Two things it must not blur:
   *
   * * **Installed is not the same as versioned.** Three of these components
   *   reject `--version`, so they render as present with the version line
   *   explicitly marked unavailable rather than blank.
   * * **Unchecked is not the same as healthy.** `devmap doctor` needs a
   *   trusted repository to run in; without one the warnings section says so
   *   instead of showing an empty, reassuring list.
   */
  import { AlertTriangle, Check, RefreshCw } from "@lucide/svelte";
  import { formatError } from "../ui/formatError";
  import { getDevcouncilSuite } from "../codeintel/client";
  import {
    healthSummary,
    missingSummary,
    needLabel,
    presetSummary,
    versionText,
  } from "../tools/devcouncilSuite";
  import type { SuiteReport } from "../codeintel/types";

  let {
    /** Where the read-only `devmap doctor` probe runs. */
    repoPath = null,
    active = true,
  }: { repoPath?: string | null; active?: boolean } = $props();

  let report = $state<SuiteReport | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(false);

  const health = $derived(report ? healthSummary(report) : null);

  async function refresh() {
    if (loading) return;
    loading = true;
    try {
      report = await getDevcouncilSuite(repoPath);
      loadError = null;
    } catch (error) {
      report = null;
      loadError = formatError(error);
    } finally {
      loading = false;
    }
  }

  // Probe when the section is *shown*, not when the modal mounts.
  //
  // Settings mounts once, on whichever section was last open, so an
  // `onMount`-only probe left this panel saying "Probing installed
  // components…" forever for every user whose last section was not Agents —
  // which is almost everyone, since this section is where they are going, not
  // where they were. Guarded on `report` so re-entering the section does not
  // re-spawn a child process every time; the Re-check button is the deliberate
  // way to ask again.
  $effect(() => {
    if (active && !report && !loading && !loadError) void refresh();
  });
</script>

<div class="space-y-2" data-testid="devcouncil-suite">
  <div class="flex items-center justify-between gap-2">
    <h4 class="text-textPrimary text-[11px] font-medium">DevCouncil components</h4>
    <button
      type="button"
      class="gp-btn py-0.5! px-2! text-[10px] inline-flex items-center gap-1"
      disabled={loading}
      onclick={() => void refresh()}
    >
      <RefreshCw size={10} class={loading ? "animate-spin" : undefined} />
      Re-check
    </button>
  </div>

  {#if loadError}
    <p class="text-amber-600 dark:text-amber-400 text-[10px]">{loadError}</p>
  {:else if !report}
    <p class="text-textMuted text-[10px]">Probing installed components…</p>
  {:else}
    <p
      class="text-[10px] leading-snug {report.suite.complete
        ? 'text-textMuted'
        : 'text-amber-600 dark:text-amber-400'}"
      data-testid="suite-missing-summary"
    >
      {missingSummary(report.suite.components)}
    </p>

    <ul class="space-y-1.5">
      {#each report.suite.components as component (component.id)}
        <li class="rounded-xl border border-border/70 bg-background/60 p-2 space-y-0.5">
          <div class="flex items-start gap-2">
            <span class="mt-0.5 shrink-0">
              {#if component.installed}
                <Check size={11} class="text-emerald-600 dark:text-emerald-400" />
              {:else if component.need === "optional"}
                <span class="text-textMuted text-[11px] leading-none">—</span>
              {:else}
                <AlertTriangle size={11} class="text-amber-600 dark:text-amber-400" />
              {/if}
            </span>
            <div class="min-w-0 flex-1">
              <div class="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                <span class="text-textPrimary text-[11px] font-mono font-medium"
                  >{component.id}</span
                >
                <span class="text-textMuted text-[10px]">{needLabel(component.need)}</span>
              </div>
              <p class="text-textMuted text-[10px] leading-snug">{component.purpose}</p>
              {#if component.installed}
                <p class="text-textMuted text-[10px] font-mono break-all leading-snug opacity-80">
                  {versionText(component.version)}
                </p>
                {#if component.path}
                  <p class="text-textMuted text-[10px] font-mono break-all leading-snug opacity-60">
                    {component.path}
                  </p>
                {/if}
              {:else if component.reason}
                <p class="text-[10px] leading-snug {component.need === 'optional' ? 'text-textMuted' : 'text-amber-600 dark:text-amber-400'}">
                  {component.reason}
                </p>
              {/if}
            </div>
          </div>
        </li>
      {/each}
    </ul>

    <div class="space-y-0.5">
      <h5 class="text-textPrimary text-[10px] font-medium">Install presets</h5>
      {#each report.suite.presets as preset (preset.id)}
        <p class="text-textMuted text-[10px] leading-snug">
          <span class="font-mono">{preset.id}</span>: {presetSummary(preset)}
        </p>
      {/each}
    </div>

    <div class="space-y-0.5">
      <h5 class="text-textPrimary text-[10px] font-medium">Installation health</h5>
      {#if health?.kind === "unchecked"}
        <p class="text-textMuted text-[10px] leading-snug" data-testid="doctor-unchecked">
          Not checked — {health.reason}
        </p>
      {:else if health?.kind === "clean"}
        <p class="text-textMuted text-[10px] leading-snug">
          <span class="font-mono">devmap doctor</span> found nothing to report.
        </p>
      {:else if health}
        <ul class="space-y-1">
          {#each health.warnings as warning, i (`${i}:${warning.slice(0, 40)}`)}
            <li
              class="text-amber-600 dark:text-amber-400 text-[10px] leading-snug break-words"
            >
              {warning}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>
