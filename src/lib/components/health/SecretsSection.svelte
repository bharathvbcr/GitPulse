<script module lang="ts">
  import { createRepoPanelCache } from "../../panels/repoPanelCache";
  import { invoke } from "../../ipc/invoke";
  import { parseSecretsReport, type SecretsReport } from "../../secrets/types";
  import { shouldCache } from "../../secrets/summary";

  // Survives the per-tab remount so revisiting Health renders the last scan
  // instantly. Written by every scan that finishes, including one whose
  // section was closed before it returned, so leaving mid-scan does not throw
  // the result away.
  const secretsCache = createRepoPanelCache<SecretsReport>();

  /**
   * The Kingfisher scan in flight for each repository.
   *
   * Module-level, not per instance. Health renders this section from more
   * than one branch (the local scan loading, failed, or loaded), so a
   * dependency scan that finishes remounts it. A per-instance guard let the
   * new instance see no cache and start a second full Kingfisher scan beside
   * the first; now it joins the one already running.
   */
  const inflightScans = new Map<string, Promise<SecretsReport>>();

  function scanRepository(repoPath: string): Promise<SecretsReport> {
    const running = inflightScans.get(repoPath);
    if (running) return running;
    const started = invoke<unknown>("cmd_scan_secrets", { repoPath })
      .then((payload) => {
        const next = parseSecretsReport(payload);
        if (shouldCache(secretsCache.get(repoPath), next)) secretsCache.set(repoPath, next);
        return next;
      })
      .finally(() => {
        if (inflightScans.get(repoPath) === started) inflightScans.delete(repoPath);
      });
    inflightScans.set(repoPath, started);
    return started;
  }
</script>

<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import {
    RefreshCw,
    AlertTriangle,
    LoaderCircle,
    ShieldCheck,
    FolderSearch,
    Clipboard,
    Check,
    Terminal,
  } from "@lucide/svelte";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { keyedList } from "../../ui/eachKeys";
  import { previewSlice } from "../../ui/previewList";
  import EmptyState from "../EmptyState.svelte";
  import HealthSection from "./HealthSection.svelte";
  import HealthPreviewToggle from "./HealthPreviewToggle.svelte";
  import { HEALTH_PREVIEW_ROWS } from "../../health/sections";
  import type { SectionCount } from "../../health/counts";
  import type { HealthTone } from "../../health/summary";
  import {
    LOCATION_COPY,
    capNote,
    distinctSecrets,
    filterFindings,
    findingKey,
    groupSizes,
    isStale,
    locationChips,
    locationLabel,
    scopeNotes,
    scanFailureCopy,
    verdict,
    type LocationFilter,
  } from "../../secrets/summary";
  import { copyText } from "../../desktop/clipboard";
  import { formatError } from "../../ui/formatError";
  import { formatRelativeTime, plural } from "../../format";
  import { createAdaptiveTimer } from "../../runtime/adaptiveTimer";
  import { revealInFileManager } from "../../desktop/openInShell";
  import { toastStore } from "../../stores/toastStore";

  /**
   * Working-tree secrets, as a section of Health.
   *
   * Kingfisher findings located against Git: tracked, untracked, ignored,
   * nested repository. No secret value is ever read or shown — a finding is a
   * rule, a path and a line.
   *
   * The scan is this section's own, independent of the dependency scan above
   * it: a failed npm audit says nothing about secrets, so the section renders
   * in every Health state that has a repository.
   */
  let {
    /**
     * The report on screen, for the Health summary and the agent prompt.
     * Null until a scan of the current repository has been shown.
     */
    report = $bindable(null),
    /** Verdict tone from the Health summary, for the heading chip. */
    tone = null,
    /**
     * Hands these findings to Health's coding-agent bar. Absent where no
     * agent bar exists to receive them, and then no button is offered.
     */
    onagent,
  }: {
    report?: SecretsReport | null;
    tone?: HealthTone | null;
    onagent?: () => void;
  } = $props();

  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  let filter = $state<LocationFilter>("all");
  let expanded = $state(false);
  let nowMs = $state(Date.now());
  let failureCopied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(copiedTimer));

  let inflight: AsyncGuard | null = null;

  // Keeps "scanned 3m ago" true while the section stays open.
  const stopTick = createAdaptiveTimer(() => (nowMs = Date.now()), 30_000);
  onDestroy(stopTick);

  // The effect below must re-run when the repository changes and at no other
  // time. Reading `$repoStore.currentPath` inside it subscribes to the whole
  // store, so every status or branch refresh during the first scan re-ran it
  // and started a second full Kingfisher scan.
  const currentPath = $derived($currentRepoPath);

  const findings = $derived(report?.findings ?? []);
  const chips = $derived(locationChips(findings));
  const shownRows = $derived(filterFindings(findings, filter));
  const previewRows = $derived(previewSlice(shownRows, expanded, HEALTH_PREVIEW_ROWS));
  const sizes = $derived(groupSizes(findings));
  const headline = $derived(report ? verdict(report) : null);
  const cappedNote = $derived(report ? capNote(report) : null);
  const scannedAgo = $derived(
    report && report.scanned_at_ms > 0
      ? formatRelativeTime(Math.floor(report.scanned_at_ms / 1000), Math.floor(nowMs / 1000))
      : "",
  );
  /**
   * The heading count. `total` is Kingfisher's own count and a floor when it
   * dropped or could not read rows, so the heading never states only the rows
   * that reached the table.
   */
  const count = $derived.by<SectionCount | null>(() => {
    if (!report || report.findings_total === 0) return null;
    return {
      shown: report.findings.length,
      total: report.findings_total,
      atLeast: report.findings_truncated || report.findings_unreadable > 0,
    };
  });

  async function scan(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    try {
      const next = await scanRepository(repoPath);
      if (!next.ok) recordFailure(scanFailureCopy(next));
      if (!guard.isLive()) return;
      report = next;
      nowMs = Date.now();
    } catch (err) {
      if (!guard.isLive()) return;
      // Keep the last report on screen: its age is printed beside it, and
      // the error says this refresh did not replace it.
      errorMsg = formatError(err);
      recordFailure(errorMsg);
    } finally {
      // A cancelled scan must still release the spinner when nothing newer
      // has taken over; otherwise a closed-then-reopened path spins forever.
      if (inflight === guard) loading = false;
    }
  }

  $effect(() => {
    const path = currentPath;
    filter = "all";
    expanded = false;
    if (!path) {
      report = null;
      errorMsg = null;
      loading = false;
      return;
    }
    const cached = secretsCache.get(path);
    report = cached ?? null;
    errorMsg = null;
    // First open of this repo, a scan already running for it, or a cached
    // result old enough that rendering it as the answer would be stale: scan
    // (joining the running one). A fresh cache is shown as-is, with its age.
    untrack(() => {
      if (!cached || inflightScans.has(path) || isStale(cached, Date.now())) void scan(path);
    });
    return () => {
      inflight?.cancel();
    };
  });

  function recordFailure(text: string) {
    void import("../../diagnostics/diagnostics")
      .then(({ diagnostics, redactDiagnosticText }) => {
        diagnostics.warn("secrets", redactDiagnosticText(text));
      })
      .catch(() => {
        // The banner already shows the failure. A diagnostics import that
        // cannot load must not replace it.
      });
  }

  async function copyFailure(text: string) {
    try {
      const { redactDiagnosticText } = await import("../../diagnostics/diagnostics");
      if (await copyText(redactDiagnosticText(text))) {
        failureCopied = true;
        clearTimeout(copiedTimer);
        copiedTimer = setTimeout(() => (failureCopied = false), 2_000);
        return;
      }
    } catch {
      // Fall through to the toast. A failed import must not reject the click.
    }
    toastStore.error("Could not copy the secrets scan error");
  }

  async function reveal(path: string) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    try {
      await revealInFileManager(repo, path);
    } catch (err) {
      toastStore.error(`Could not reveal ${path}: ${formatError(err)}`);
    }
  }

  const toneClass = {
    ok: "border-emerald-500/30 bg-emerald-500/10 text-emerald-100",
    warn: "border-amber-500/30 bg-amber-500/10 text-amber-100",
    danger: "border-rose-500/30 bg-rose-500/10 text-rose-100",
  } as const;

  const badgeClass = {
    danger: "bg-rose-500/15 text-rose-200 border-rose-500/30",
    warn: "bg-amber-500/15 text-amber-200 border-amber-500/30",
    muted: "bg-surface/80 text-textMuted border-border/60",
  } as const;
</script>

{#snippet copyErrorButton(text: string, label: string)}
  <button
    type="button"
    class="gp-btn py-0.5! px-2! text-[10px]! shrink-0"
    title={label}
    aria-label={label}
    onclick={() => void copyFailure(text)}
  >
    {#if failureCopied}
      <Check size={10} />
      <span>Copied</span>
    {:else}
      <Clipboard size={10} />
      <span>Copy error</span>
    {/if}
  </button>
{/snippet}

<HealthSection id="secrets" {count} {tone}>
  {#snippet actions()}
    <div class="flex items-center gap-2 min-w-0 flex-wrap justify-end">
      {#if report}
        <span class="text-[11px] text-textMuted" data-testid="secrets-meta">
          {#if !report.ok}<span class="text-amber-300">Unavailable</span>{/if}
          {#if scannedAgo}{report.ok ? "" : "· "}scanned {scannedAgo}{/if}
          {#if report.kingfisher_version}· Kingfisher {report.kingfisher_version}{/if}
          {#if report.kingfisher_version_rejected}<span
              class="text-amber-300"
              title="The scan output named a version that was refused as unsafe to display.">
              · scan-reported version refused</span
            >{/if}
        </span>
      {/if}
      {#if onagent && $repoStore.currentPath}
        <button
          type="button"
          class="gp-btn py-1! px-2.5! text-xs! inline-flex items-center gap-1.5"
          title="Choose a coding agent for these findings in the agent bar above"
          onclick={() => onagent?.()}
        >
          <Terminal size={13} />
          Fix with agent
        </button>
      {/if}
      <button
        type="button"
        class="gp-btn py-1! px-2.5! text-xs! inline-flex items-center gap-1.5"
        onclick={() => scan()}
        disabled={loading || !$repoStore.currentPath}
        aria-label="Rescan secrets"
      >
        {#if loading}
          <LoaderCircle size={13} class="animate-spin" />
          {report ? "Rescanning…" : "Scanning…"}
        {:else}
          <RefreshCw size={13} />
          Rescan
        {/if}
      </button>
    </div>
  {/snippet}

  {#if !$repoStore.currentPath}
    <p class="text-textMuted">No repository open. Open one to scan its working tree with Kingfisher.</p>
  {:else if loading && !report}
    <div class="flex items-center gap-2 text-textMuted text-xs px-1 py-3">
      <LoaderCircle size={14} class="animate-spin" />
      Scanning working tree…
    </div>
  {:else if errorMsg && !report}
    <div
      class="rounded-lg border border-rose-500/30 bg-rose-500/10 px-3 py-2 text-xs text-rose-200 flex items-start gap-2"
      role="alert"
    >
      <AlertTriangle size={14} class="shrink-0 mt-0.5" />
      <span class="select-text min-w-0 flex-1">{errorMsg}</span>
      {@render copyErrorButton(errorMsg ?? "", "Copy secrets scan error")}
    </div>
  {:else if report && headline}
    {#if errorMsg}
      <div
        class="rounded-lg border border-rose-500/30 bg-rose-500/10 px-3 py-2 text-xs text-rose-200 flex items-start gap-2"
        role="alert"
      >
        <AlertTriangle size={14} class="shrink-0 mt-0.5" />
        <span class="select-text min-w-0 flex-1">
          Rescan failed: {errorMsg}
          {#if scannedAgo}Showing the scan from {scannedAgo}.{/if}
        </span>
        {@render copyErrorButton(errorMsg ?? "", "Copy rescan error")}
      </div>
    {/if}

    <div
      class="rounded-lg border px-3 py-2 text-xs flex items-start gap-2 {toneClass[headline.tone]}"
      role={headline.tone === "ok" ? "status" : "alert"}
      data-testid="secrets-verdict"
      data-tone={headline.tone}
    >
      {#if headline.tone === "ok"}
        <ShieldCheck size={14} class="shrink-0 mt-0.5" />
      {:else}
        <AlertTriangle size={14} class="shrink-0 mt-0.5" />
      {/if}
      <div class="space-y-1 min-w-0 flex-1">
        <p class="font-medium">
          {headline.title}{#if findings.length > 0}<span class="font-normal opacity-80"
              >{` · ${plural(distinctSecrets(findings), "distinct value")} shown`}</span
            >{/if}
        </p>
        {#if headline.detail}<p class="opacity-80 select-text">{headline.detail}</p>{/if}
        {#if report.diagnostic}
          <pre
            class="whitespace-pre-wrap wrap-break-word font-mono text-[10px] leading-relaxed select-text opacity-90"
            data-testid="secrets-diagnostic">{report.diagnostic}</pre>
        {/if}
        {#if cappedNote}<p class="opacity-80">{cappedNote}</p>{/if}
      </div>
      {#if !report.ok}
        {@render copyErrorButton(scanFailureCopy(report), "Copy secrets scan error")}
      {/if}
    </div>

    {#if chips.length > 0}
      <div class="flex flex-wrap items-center gap-1.5" role="group" aria-label="Filter findings by location">
        <button
          type="button"
          class="text-[11px] px-2 py-0.5 rounded-full border {filter === 'all'
            ? 'border-accent/60 text-textPrimary bg-accent/10'
            : 'border-border/60 text-textMuted'}"
          aria-pressed={filter === "all"}
          onclick={() => (filter = "all")}
        >
          All {findings.length}
        </button>
        {#each chips as chip (chip.location)}
          <button
            type="button"
            class="text-[11px] px-2 py-0.5 rounded-full border {filter === chip.location
              ? 'border-accent/60 text-textPrimary bg-accent/10'
              : badgeClass[LOCATION_COPY[chip.location].tone]}"
            aria-pressed={filter === chip.location}
            title={LOCATION_COPY[chip.location].hint}
            onclick={() => (filter = filter === chip.location ? "all" : chip.location)}
          >
            {LOCATION_COPY[chip.location].label} {chip.count}
          </button>
        {/each}
      </div>

      <div class="rounded-lg border border-border/60 overflow-hidden">
        <table class="w-full text-xs table-fixed">
          <caption class="sr-only">Secret findings in the working tree</caption>
          <thead class="bg-surface/80 text-textMuted text-[10px] uppercase tracking-wide">
            <tr>
              <th class="text-left font-semibold px-3 py-2 w-28">Location</th>
              <th class="text-left font-semibold px-3 py-2">File</th>
              <th class="text-left font-semibold px-3 py-2 w-48">Rule</th>
              <th class="text-left font-semibold px-3 py-2 w-20">Confidence</th>
              <th class="px-2 py-2 w-10"><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each keyedList(previewRows, findingKey) as { item: finding, key } (key)}
              {@const copy = LOCATION_COPY[finding.location]}
              {@const sameValue = sizes.get(finding.secret_group) ?? 0}
              <tr class="border-t border-border/40 align-top" data-location={finding.location}>
                <td class="px-3 py-1.5">
                  <span
                    class="inline-block whitespace-nowrap rounded border px-1.5 py-px text-[10px] {badgeClass[copy.tone]}"
                    title={copy.hint}>{copy.label}</span
                  >
                </td>
                <td class="px-3 py-1.5 font-mono text-textPrimary break-all">
                  <span title={finding.path}>{finding.path}</span>{#if finding.line !== null}<span
                      class="text-textMuted">:{finding.line}</span
                    >{/if}
                  {#if sameValue > 1}
                    <span
                      class="ml-1 font-sans text-[10px] text-amber-300 whitespace-nowrap"
                      title="Kingfisher matched the same value in {sameValue} shown locations; rotating it means fixing all of them."
                      >same value ×{sameValue}</span
                    >
                  {/if}
                </td>
                <td class="px-3 py-1.5 min-w-0">
                  <div class="text-textPrimary truncate" title={finding.rule_id}>
                    {finding.rule_name || finding.rule_id}
                  </div>
                  {#if finding.rule_name}
                    <div class="font-mono text-[10px] text-textMuted truncate">{finding.rule_id}</div>
                  {/if}
                </td>
                <td class="px-3 py-1.5 text-textMuted capitalize">{finding.confidence || "—"}</td>
                <td class="px-2 py-1.5 text-right">
                  {#if finding.location !== "outside"}
                    <button
                      type="button"
                      class="gp-icon-btn p-1 rounded hover:bg-surface"
                      aria-label="Reveal {locationLabel(finding)} in file manager"
                      title="Reveal in file manager"
                      onclick={() => reveal(finding.path)}
                    >
                      <FolderSearch size={13} />
                    </button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <HealthPreviewToggle total={shownRows.length} noun="findings" bind:expanded />
    {:else if report.ok && headline.tone === "ok"}
      <EmptyState
        icon={ShieldCheck}
        title="Nothing to rotate"
        hint="No secret matched in the files Kingfisher could read. The scope below says what that covers."
        compact
      />
    {/if}

    {#if report.kingfisher_present}
      <details class="text-[11px] text-textMuted px-0.5" data-testid="secrets-scope">
        <summary class="cursor-pointer select-none">What this scan covers</summary>
        <ul class="mt-1.5 space-y-0.5 list-disc pl-4">
          {#each scopeNotes(report) as note (note)}
            <li>{note}</li>
          {/each}
        </ul>
      </details>
    {/if}
  {/if}
</HealthSection>
