<script lang="ts">
  /**
   * Register DevMap with an agent host for this repository — previewed first.
   *
   * The counterpart to the automatic setup in `codeintel/autoInit.ts`, and the
   * split between them is what the user sees in `git status`. Auto-init writes
   * only machine state, so it runs on open. Everything here writes tracked
   * files — `AGENTS.md`, `CLAUDE.md`, `.cursor/rules/devmap.mdc`, `.mcp.json`,
   * skills, hook config — and a machine-wide MCP registration in the user's
   * home directory, so nothing here happens without a click on a preview the
   * user has read.
   *
   * The two change counts stay separate on purpose. `devmap integrate` has no
   * flag to write the project assets without the global registration, so
   * applying is all-or-nothing; agreeing to "add agent guides to this project"
   * is not agreeing to edit `~/.claude.json`, and one merged total would hide
   * that.
   */
  import { AlertTriangle, Check, ChevronRight, RefreshCw } from "@lucide/svelte";
  import { formatError } from "../ui/formatError";
  import { displayName } from "../repos/paths";
  import {
    applyDevmapIntegration,
    previewDevmapIntegration,
    surveyDevmapIntegration,
  } from "../codeintel/client";
  import {
    acceptPreview,
    applyConfirmText,
    canApply,
    hostLabel,
    indexPlansByHost,
    INTEGRATION_HOSTS,
    kindLabel,
    planSummary,
    totalChanges,
  } from "../tools/agentIntegration";
  import type { IntegrationHost, IntegrationPlan } from "../codeintel/types";

  let { repoPath = null }: { repoPath?: string | null } = $props();

  // Keyed by the host that was *asked about*. The payload's own `host` is
  // never a row identity: one mismatched answer used to file two plans under
  // the same key, and a keyed `each` with duplicates throws and takes the
  // panel down.
  let plans = $state<Map<IntegrationHost, IntegrationPlan>>(new Map());
  let surveyed = $state<string | null>(null);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let expanded = $state<IntegrationHost | null>(null);
  let applying = $state<IntegrationHost | null>(null);
  let applied = $state<IntegrationPlan | null>(null);

  const rows = $derived(
    INTEGRATION_HOSTS.map((host) => ({ host, plan: plans.get(host) })).filter(
      (row): row is { host: IntegrationHost; plan: IntegrationPlan } => row.plan !== undefined,
    ),
  );

  async function survey(path: string) {
    if (loading) return;
    loading = true;
    applied = null;
    try {
      plans = indexPlansByHost(INTEGRATION_HOSTS, await surveyDevmapIntegration(path));
      surveyed = path;
      loadError = null;
    } catch (error) {
      plans = new Map();
      loadError = formatError(error);
    } finally {
      loading = false;
    }
  }

  async function toggle(host: IntegrationHost) {
    if (expanded === host) {
      expanded = null;
      return;
    }
    expanded = host;
    // Re-read on expand: the repository may have changed since the survey, and
    // a stale preview is what an apply would then be consented against.
    if (!repoPath) return;
    try {
      const fresh = acceptPreview(host, await previewDevmapIntegration(repoPath, host));
      if ("mismatch" in fresh) {
        loadError = fresh.mismatch;
        return;
      }
      plans = new Map(plans).set(host, fresh);
      loadError = null;
    } catch (error) {
      loadError = formatError(error);
    }
  }

  async function apply(host: IntegrationHost) {
    const path = repoPath;
    const plan = plans.get(host);
    if (!path || !plan || applying) return;
    if (typeof window !== "undefined" && !window.confirm(applyConfirmText(plan))) {
      return;
    }
    applying = host;
    try {
      const outcome = acceptPreview(host, await applyDevmapIntegration(path, host));
      if ("mismatch" in outcome) {
        loadError = outcome.mismatch;
        return;
      }
      applied = outcome;
      plans = new Map(plans).set(host, outcome);
      loadError = outcome.available ? null : (outcome.reason ?? "Integration failed");
      // The applied plan describes what was written; re-survey so the row goes
      // back to describing what is left to do.
      await survey(path);
      expanded = host;
    } catch (error) {
      loadError = formatError(error);
    } finally {
      applying = null;
    }
  }

  $effect(() => {
    const path = repoPath;
    if (!path || path === surveyed) return;
    void survey(path);
  });
</script>

<div class="space-y-2" data-testid="agent-integration">
  <div class="flex items-center justify-between gap-2">
    <h4 class="text-textPrimary text-[11px] font-medium">Agent host integration</h4>
    <button
      type="button"
      class="gp-btn py-0.5! px-2! text-[10px] inline-flex items-center gap-1"
      disabled={loading || !repoPath}
      onclick={() => repoPath && void survey(repoPath)}
    >
      <RefreshCw size={10} class={loading ? "animate-spin" : undefined} />
      Re-check
    </button>
  </div>

  <p class="text-textMuted text-[10px] leading-snug">
    Writes agent guides, editor rules, MCP entries and skills into
    {#if repoPath}<span class="font-mono">{displayName(repoPath)}</span>{:else}the open
      repository{/if}. Previewed first; nothing is written until you apply.
  </p>

  {#if loadError}
    <p class="text-amber-600 dark:text-amber-400 text-[10px]">{loadError}</p>
  {/if}

  {#if !repoPath}
    <p class="text-textMuted text-[10px]">Open a repository to check its agent integration.</p>
  {:else if loading && plans.size === 0}
    <p class="text-textMuted text-[10px]">Checking…</p>
  {:else}
    <ul class="space-y-1.5">
      {#each rows as row (row.host)}
        {@const host = row.host}
        {@const plan = row.plan}
        {@const total = totalChanges(plan)}
        <li class="rounded-xl border border-border/70 bg-background/60">
          <button
            type="button"
            class="flex w-full items-start gap-2 p-2 text-left"
            aria-expanded={expanded === host}
            onclick={() => void toggle(host)}
          >
            <span class="mt-0.5 shrink-0">
              {#if !plan.available}
                <AlertTriangle size={11} class="text-amber-600 dark:text-amber-400" />
              {:else if total === 0}
                <Check size={11} class="text-emerald-600 dark:text-emerald-400" />
              {:else}
                <ChevronRight
                  size={11}
                  class="text-textMuted transition-transform {expanded === host
                    ? 'rotate-90'
                    : ''}"
                />
              {/if}
            </span>
            <span class="min-w-0 flex-1">
              <span class="text-textPrimary block text-[11px] font-medium"
                >{hostLabel(host)}</span
              >
              <span class="text-textMuted block text-[10px] leading-snug">
                {planSummary(plan)}
              </span>
            </span>
          </button>

          {#if expanded === host && plan.available}
            <div class="border-border/70 space-y-1.5 border-t px-2 pt-1.5 pb-2">
              {#if plan.entries.length === 0}
                <p class="text-textMuted text-[10px]">Nothing to write.</p>
              {:else}
                <ul class="space-y-0.5">
                  {#each plan.entries as entry, i (`${entry.kind}:${entry.path}:${i}`)}
                    <li class="text-[10px] leading-snug">
                      <span
                        class="font-medium {entry.changed
                          ? 'text-textPrimary'
                          : 'text-textMuted'}"
                      >
                        {kindLabel(entry.kind)}
                      </span>
                      <span class="text-textMuted"> · {entry.disposition}</span>
                      {#if entry.outside_repo}
                        <span class="text-amber-600 dark:text-amber-400">
                          · outside this repository</span
                        >
                      {/if}
                      <span class="text-textMuted block font-mono break-all opacity-70"
                        >{entry.path}</span
                      >
                      {#if entry.note}
                        <span class="text-textMuted block leading-snug opacity-80">{entry.note}</span
                        >
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}

              {#if plan.protected.length > 0}
                <p class="text-textMuted text-[10px] leading-snug">
                  Left alone because you wrote {plan.protected.length === 1 ? "it" : "them"}:
                  <span class="font-mono break-all">{plan.protected.join(", ")}</span>
                </p>
              {/if}

              {#each plan.notes as note, i (`${i}:${note.slice(0, 40)}`)}
                <p class="text-textMuted text-[10px] leading-snug">{note}</p>
              {/each}

              {#if plan.applied}
                <p class="text-[10px] leading-snug text-emerald-700 dark:text-emerald-300">
                  Written.
                </p>
              {/if}

              <button
                type="button"
                class="gp-btn py-0.5! px-2! text-[10px]"
                disabled={applying !== null || !canApply(plan)}
                onclick={() => void apply(host)}
              >
                {applying === host ? "Writing…" : "Apply"}
              </button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if applied && !applied.available}
    <p class="text-[10px] text-rose-700 dark:text-rose-300">
      {applied.reason ?? "Integration failed"}
    </p>
  {/if}
</div>
