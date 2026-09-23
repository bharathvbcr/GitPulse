<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { SecretsReport } from "../secrets/types";

  // Survives the per-tab remount so revisiting Secrets renders the last scan
  // instantly; the fetch then refreshes it in place.
  const secretsCache = createRepoPanelCache<SecretsReport>();
</script>

<script lang="ts">
  import { untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { KeyRound, RefreshCw, AlertTriangle, LoaderCircle } from "@lucide/svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { keyedList } from "../ui/eachKeys";
  import EmptyState from "./EmptyState.svelte";
  import { parseSecretsReport } from "../secrets/types";
  import { formatError } from "../ui/formatError";

  let report = $state<SecretsReport | null>(null);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);

  const scanned = { path: "" };
  let inflight: AsyncGuard | null = null;

  const findingRows = $derived(report?.findings ?? []);

  async function scan(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    try {
      const raw = await invoke<unknown>("cmd_scan_secrets", { repoPath });
      if (!guard.isLive()) return;
      const next = parseSecretsReport(raw);
      report = next;
      secretsCache.set(repoPath, next);
      scanned.path = repoPath;
    } catch (err) {
      if (!guard.isLive()) return;
      errorMsg = formatError(err);
      report = null;
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      report = null;
      errorMsg = null;
      loading = false;
      scanned.path = "";
      return;
    }
    const cached = secretsCache.get(path);
    if (cached) {
      report = cached;
      scanned.path = path;
      errorMsg = null;
    } else if (scanned.path !== path) {
      report = null;
    }
    // Auto-scan on first open of this repo's Secrets section, matching Health.
    untrack(() => {
      if (scanned.path !== path || !cached) {
        void scan(path);
      }
    });
    return () => {
      inflight?.cancel();
    };
  });
</script>

<div class="flex-1 flex flex-col min-h-0">
  <div
    class="px-4 py-2 border-b border-border/60 gp-section-edge bg-surface/60 flex items-center justify-between gap-3 shrink-0"
  >
    <div class="flex items-center gap-2 min-w-0">
      <KeyRound size={16} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary shrink-0">Secrets</span>
      {#if report?.ok}
        <span class="text-[11px] text-textMuted truncate">
          {report.findings.length}
          {report.findings.length === 1 ? "finding" : "findings"}
          {#if report.findings_truncated}+{/if}
          {#if report.kingfisher_version}
            · Kingfisher {report.kingfisher_version}
          {/if}
        </span>
      {:else if report && !report.ok}
        <span class="text-[11px] text-amber-300 truncate">Unavailable</span>
      {/if}
    </div>
    <button
      type="button"
      class="gp-btn py-1! px-2.5! text-xs! inline-flex items-center gap-1.5"
      onclick={() => scan()}
      disabled={loading || !$repoStore.currentPath}
      aria-label="Rescan secrets"
    >
      {#if loading}
        <LoaderCircle size={13} class="animate-spin" />
      {:else}
        <RefreshCw size={13} />
      {/if}
      Rescan
    </button>
  </div>

  <div class="flex-1 overflow-auto p-4 space-y-3">
    {#if !$repoStore.currentPath}
      <EmptyState
        icon={KeyRound}
        title="No repository open"
        hint="Open a repository to scan the working tree for secrets with Kingfisher."
      />
    {:else if loading && !report}
      <div class="flex items-center gap-2 text-textMuted text-xs px-1 py-6 justify-center">
        <LoaderCircle size={14} class="animate-spin" />
        Scanning working tree…
      </div>
    {:else if errorMsg && !report}
      <div
        class="rounded-lg border border-rose-500/30 bg-rose-500/10 px-3 py-2 text-xs text-rose-200 flex items-start gap-2"
        role="alert"
      >
        <AlertTriangle size={14} class="shrink-0 mt-0.5" />
        <span>{errorMsg}</span>
      </div>
    {:else if report && !report.ok}
      <div
        class="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-100 flex items-start gap-2"
        role="status"
      >
        <AlertTriangle size={14} class="shrink-0 mt-0.5" />
        <div class="space-y-1 min-w-0">
          <p class="font-medium">Secrets scan unavailable</p>
          <p class="text-amber-100/80">
            {report.error ?? "The scanner did not complete. This is not a clean result."}
          </p>
        </div>
      </div>
    {:else if report?.ok}
      {#if report.findings_truncated}
        <div
          class="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-100 flex items-start gap-2"
          role="status"
        >
          <AlertTriangle size={14} class="shrink-0 mt-0.5" />
          <span>
            Kingfisher omitted some findings past its report cap. The list below is a
            floor, not the complete set.
          </span>
        </div>
      {/if}
      {#if report.nested_repos_scanned}
        <p class="text-[11px] text-textMuted px-0.5">
          Nested git repositories inside this tree are also scanned. Scope is not
          limited to this worktree alone.
        </p>
      {/if}
      {#if findingRows.length === 0}
        <EmptyState
          icon={KeyRound}
          title="No secrets reported"
          hint="Kingfisher finished with no findings in the working tree at medium confidence and above."
          compact
        />
      {:else}
        <div class="rounded-lg border border-border/60 overflow-hidden">
          <table class="w-full text-xs">
            <thead class="bg-surface/80 text-textMuted text-[10px] uppercase tracking-wide">
              <tr>
                <th class="text-left font-semibold px-3 py-2">Path</th>
                <th class="text-left font-semibold px-3 py-2 w-16">Line</th>
                <th class="text-left font-semibold px-3 py-2">Rule</th>
              </tr>
            </thead>
            <tbody>
              {#each keyedList(findingRows, (f) => `${f.fingerprint}:${f.path}:${f.line}:${f.rule_id}`) as { item: finding, key } (key)}
                <tr class="border-t border-border/40 align-top">
                  <td class="px-3 py-1.5 font-mono text-textPrimary break-all">{finding.path || "—"}</td>
                  <td class="px-3 py-1.5 font-mono text-textMuted">
                    {finding.line > 0 ? finding.line : "—"}
                  </td>
                  <td class="px-3 py-1.5 font-mono text-textPrimary">{finding.rule_id}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    {/if}
  </div>
</div>
