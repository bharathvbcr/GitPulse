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
  import { describeFailure } from "../ui/copy/describeFailure";
  import Skeleton from "./Skeleton.svelte";
  import { askConfirm } from "../stores/modalStore";
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
  /** Checking and previewing that could not run are "unknown"; a write that failed is "danger". */
  let loadErrorTone = $state<"unknown" | "danger">("unknown");

  /** "Couldn't <verb> <object>. <Reason>." for an error line. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

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
      loadErrorTone = "unknown";
      loadError = failure("check", "the agent integration", error);
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
      loadErrorTone = "unknown";
      loadError = failure("preview", "the integration changes", error);
    }
  }

  async function apply(host: IntegrationHost) {
    const path = repoPath;
    const plan = plans.get(host);
    if (!path || !plan || applying) return;
    // The app's own dialog: the native confirm answers null under WKWebView,
    // which read as "no" and left Apply doing nothing.
    if (!(await askConfirm({ title: "Apply integration changes?", message: applyConfirmText(plan), confirmLabel: "Apply" }))) return;
    applying = host;
    try {
      const outcome = acceptPreview(host, await applyDevmapIntegration(path, host));
      if ("mismatch" in outcome) {
        loadError = outcome.mismatch;
        return;
      }
      applied = outcome;
      plans = new Map(plans).set(host, outcome);
      loadErrorTone = "danger";
      loadError = outcome.available ? null : (outcome.reason ?? "Couldn't apply the integration.");
      // The applied plan describes what was written; re-survey so the row goes
      // back to describing what is left to do.
      await survey(path);
      expanded = host;
    } catch (error) {
      loadErrorTone = "danger";
      loadError = failure("apply", "the integration", error);
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
    <h4 class="text-textPrimary text-meta font-medium">Agent host integration</h4>
    <button
      type="button"
      class="gp-btn" data-size="sm"
      disabled={loading || !repoPath}
      onclick={() => repoPath && void survey(repoPath)}
    >
      <RefreshCw size={10} class={loading ? "animate-spin" : undefined} />
      Rescan
    </button>
  </div>

  <p class="text-textMuted text-micro leading-snug">
    Writes agent guides, editor rules, MCP entries and skills into
    {#if repoPath}<span class="font-mono">{displayName(repoPath)}</span>{:else}the open
      repository{/if}. Previewed first; nothing is written until you apply.
  </p>

  {#if loadError}
    <div class="gp-notice" data-tone={loadErrorTone} role={loadErrorTone === "danger" ? "alert" : "status"}>{loadError}</div>
  {/if}

  {#if !repoPath}
    <p class="text-textMuted text-micro">Open a repository to check its agent integration.</p>
  {:else if loading && plans.size === 0}
    <Skeleton variant="text" count={3} />
  {:else}
    <ul class="space-y-1.5">
      {#each rows as row (row.host)}
        {@const host = row.host}
        {@const plan = row.plan}
        {@const total = totalChanges(plan)}
        <li class="rounded-xl bg-background/60">
          <button
            type="button"
            class="flex w-full items-start gap-2 p-2 text-left"
            aria-expanded={expanded === host}
            onclick={() => void toggle(host)}
          >
            <span class="mt-0.5 shrink-0">
              {#if !plan.available}
                <AlertTriangle size={11} class="text-warn" />
              {:else if total === 0}
                <Check size={11} class="text-ok" />
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
              <span class="text-textPrimary block text-meta font-medium"
                >{hostLabel(host)}</span
              >
              <span class="text-textMuted block text-micro leading-snug">
                {planSummary(plan)}
              </span>
            </span>
          </button>

          {#if expanded === host && plan.available}
            <div class="space-y-1.5 px-2 pt-1.5 pb-2">
              {#if plan.entries.length === 0}
                <p class="text-textMuted text-micro">Nothing to write.</p>
              {:else}
                <ul class="space-y-0.5">
                  {#each plan.entries as entry, i (`${entry.kind}:${entry.path}:${i}`)}
                    <li class="text-micro leading-snug">
                      <span
                        class="font-medium {entry.changed
                          ? 'text-textPrimary'
                          : 'text-textMuted'}"
                      >
                        {kindLabel(entry.kind)}
                      </span>
                      <span class="text-textMuted"> · {entry.disposition}</span>
                      {#if entry.outside_repo}
                        <span class="text-warn">
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
                <p class="text-textMuted text-micro leading-snug">
                  Left alone because you wrote {plan.protected.length === 1 ? "it" : "them"}:
                  <span class="font-mono break-all">{plan.protected.join(", ")}</span>
                </p>
              {/if}

              {#each plan.notes as note, i (`${i}:${note.slice(0, 40)}`)}
                <p class="text-textMuted text-micro leading-snug">{note}</p>
              {/each}

              {#if plan.applied}
                <p class="text-micro leading-snug text-ok">
                  Written.
                </p>
              {/if}

              <button
                type="button"
                class="gp-btn" data-size="sm"
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
    <div class="gp-notice" data-tone="danger" role="alert">
      {applied.reason ?? "Couldn't apply the integration."}
    </div>
  {/if}
</div>
