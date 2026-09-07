<script lang="ts">
  /**
   * Probe + install UI for `devmap` and `manvi`.
   *
   * Used from Settings → Agents, Code → Map (when missing), and the MANVI
   * harness pane. One owner so the three surfaces cannot disagree about which
   * path answered or what command runs.
   */
  import { onMount } from "svelte";
  import { Download, RefreshCw, X } from "@lucide/svelte";
  import { formatError } from "../ui/formatError";
  import {
    cancelExternalToolInstall,
    getExternalToolsStatus,
    installButtonLabel,
    installExternalTool,
    toolStatusSummary,
    type ExternalTool,
    type InstallOutcome,
    type ToolStatus,
    type ToolsStatus,
  } from "../tools/externalTools";
  import { openSetupWizard } from "../tools/onboardingStore";

  let {
    /** When set, only this tool's row is shown (Map → devmap, MANVI → manvi). */
    only = null,
    compact = false,
    onInstalled,
  }: {
    only?: ExternalTool | null;
    compact?: boolean;
    onInstalled?: (tool: ExternalTool) => void;
  } = $props();

  let status = $state<ToolsStatus | null>(null);
  let loadError = $state<string | null>(null);
  let installing = $state<ExternalTool | null>(null);
  let lastOutcome = $state<InstallOutcome | null>(null);

  const rows = $derived.by((): ToolStatus[] => {
    if (!status) return [];
    if (only === "devmap") return [status.devmap];
    if (only === "manvi") return [status.manvi];
    return [status.devmap, status.manvi];
  });

  async function refresh() {
    try {
      status = await getExternalToolsStatus();
      loadError = null;
    } catch (error) {
      status = null;
      loadError = formatError(error);
    }
  }

  async function runInstall(tool: ExternalTool) {
    if (installing) return;
    installing = tool;
    lastOutcome = null;
    try {
      const outcome = await installExternalTool(tool);
      lastOutcome = outcome;
      await refresh();
      if (outcome.ok) onInstalled?.(tool);
    } catch (error) {
      lastOutcome = {
        tool,
        ok: false,
        binary: null,
        lookup: null,
        version: null,
        source_used: null,
        command: "",
        exit_code: null,
        stdout: "",
        stderr: formatError(error),
        timed_out: false,
        cancelled: false,
        reason: formatError(error),
      };
    } finally {
      installing = null;
    }
  }

  async function cancel() {
    await cancelExternalToolInstall();
  }

  onMount(() => {
    void refresh();
  });
</script>

<div class="space-y-2">
  {#if loadError}
    <p class="text-amber-600 dark:text-amber-400 text-[10px]">{loadError}</p>
  {:else if !status}
    <p class="text-textMuted text-[10px]">Checking installed tools…</p>
  {:else}
    {#each rows as row, i (`${row.tool}#${i}`)}
      <div
        class="rounded-xl border border-border/70 bg-background/60 {compact
          ? 'p-2'
          : 'p-2.5'} space-y-1.5"
      >
        <div class="flex items-start gap-2">
          <div class="min-w-0 flex-1">
            <div class="text-textPrimary text-[11px] font-medium font-mono">{row.tool}</div>
            <div class="text-textMuted text-[10px] font-mono break-all leading-snug">
              {toolStatusSummary(row)}
            </div>
            {#if !row.installed && row.source_checkout}
              <div class="text-textMuted text-[10px] mt-0.5 break-all">
                Source: {row.source_checkout}
              </div>
            {/if}
            {#if !row.install_ready && row.install_block}
              <div class="text-amber-600 dark:text-amber-400 text-[10px] mt-0.5">
                {row.install_block}
              </div>
            {/if}
          </div>
          <div class="flex shrink-0 items-center gap-1">
            <button
              type="button"
              class="gp-btn py-0.5! px-2! text-[10px]"
              onclick={() => openSetupWizard(row.tool, "explain")}
            >
              Setup
            </button>
            {#if installing === row.tool}
              <button
                type="button"
                class="gp-btn py-0.5! px-2! text-[10px] inline-flex items-center gap-1"
                onclick={() => void cancel()}
              >
                <X size={10} />
                Cancel
              </button>
            {:else}
              <button
                type="button"
                class="gp-btn py-0.5! px-2! text-[10px] inline-flex items-center gap-1"
                disabled={!row.install_ready || installing !== null}
                title={row.install_command}
                onclick={() => void runInstall(row.tool)}
              >
                {#if row.installed}
                  <RefreshCw size={10} />
                {:else}
                  <Download size={10} />
                {/if}
                {installButtonLabel(row)}
              </button>
            {/if}
          </div>
        </div>
        {#if !compact}
          <p class="text-textMuted text-[10px] font-mono break-all leading-snug opacity-80">
            {row.install_command}
          </p>
        {/if}
      </div>
    {/each}
  {/if}

  {#if lastOutcome && (!only || lastOutcome.tool === only)}
    <div
      class="rounded-lg border px-2 py-1.5 text-[10px] leading-snug {lastOutcome.ok
        ? 'border-emerald-500/30 text-emerald-700 dark:text-emerald-300'
        : 'border-rose-500/30 text-rose-700 dark:text-rose-300'}"
    >
      {#if lastOutcome.ok}
        Installed {lastOutcome.tool} at {lastOutcome.binary}
      {:else if lastOutcome.cancelled}
        Install cancelled.
      {:else}
        {lastOutcome.reason ?? (lastOutcome.stderr || "Install failed")}
      {/if}
    </div>
  {/if}

  {#if installing}
    <p class="text-textMuted text-[10px] flex items-center gap-1.5">
      <RefreshCw size={10} class="animate-spin" />
      Installing {installing}… this can take several minutes.
    </p>
  {/if}
</div>
