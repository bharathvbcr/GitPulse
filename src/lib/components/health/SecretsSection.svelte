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
  import HealthTable from "./HealthTable.svelte";
  import Skeleton from "../Skeleton.svelte";
  import { describeFailure } from "../../ui/copy/describeFailure";
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
    toastStore.error(describeFailure({ verb: "copy", object: "the secrets scan error" }, "").headline);
  }

  async function reveal(path: string) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    try {
      await revealInFileManager(repo, path);
    } catch (err) {
      const failure = describeFailure({ verb: "reveal", object: path }, err);
      toastStore.error(failure.detail ? `${failure.headline} ${failure.detail}` : failure.headline);
    }
  }

  /** A location's tone (secrets/summary.ts vocabulary) as a `gp-chip` role. */
  const chipTone = {
    danger: "danger",
    warn: "warn",
    muted: "neutral",
  } as const;

  const SCAN_FAILED = describeFailure({ verb: "scan", object: "the working tree for secrets" }, "").headline;
  const RESCAN_FAILED = describeFailure({ verb: "rescan", object: "the working tree for secrets" }, "").headline;

  const FINDING_COLUMNS = Object.freeze([
    { label: "Location", width: "w-28" },
    { label: "File" },
    { label: "Rule", width: "w-48" },
    { label: "Confidence", width: "w-24" },
    { label: "Actions", width: "w-10", hideLabel: true },
  ]);
</script>

{#snippet copyErrorButton(text: string, label: string)}
  <button
    type="button"
    class="gp-btn shrink-0"
    data-size="sm"
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
        <span class="text-meta text-textMuted" data-testid="secrets-meta">
          {#if !report.ok}<span class="text-warn">Unavailable</span>{/if}
          {#if scannedAgo}{report.ok ? "" : "· "}scanned {scannedAgo}{/if}
          {#if report.kingfisher_version}· Kingfisher {report.kingfisher_version}{/if}
          {#if report.kingfisher_version_rejected}<span
              class="text-warn"
              title="The scan output named a version that was refused as unsafe to display.">
              · scan-reported version refused</span
            >{/if}
        </span>
      {/if}
      {#if onagent && $repoStore.currentPath}
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          title="Choose a coding agent for these findings in the agent bar above"
          onclick={() => onagent?.()}
        >
          <Terminal size={13} />
          Fix with agent
        </button>
      {/if}
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
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
    <!-- A full Kingfisher scan outlasts a second, so the wait is a skeleton;
         the spinner lives only inside the busy Rescan button above. -->
    <div class="space-y-2">
      <p class="text-textMuted">Scanning working tree…</p>
      <Skeleton variant="text" count={3} height="1.5rem" />
    </div>
  {:else if errorMsg && !report}
    <div class="gp-notice" data-tone="danger" role="alert">
      <AlertTriangle size={14} class="shrink-0 mt-0.5" style="color: var(--gp-notice-ink)" />
      <span class="select-text min-w-0 flex-1">
        <span class="font-medium">{SCAN_FAILED}</span>
        {errorMsg}
      </span>
      {@render copyErrorButton(errorMsg ?? "", "Copy secrets scan error")}
    </div>
  {:else if report && headline}
    {#if errorMsg}
      <div class="gp-notice" data-tone="danger" role="alert">
        <AlertTriangle size={14} class="shrink-0 mt-0.5" style="color: var(--gp-notice-ink)" />
        <span class="select-text min-w-0 flex-1">
          <span class="font-medium">{RESCAN_FAILED}</span>
          {errorMsg}
          {#if scannedAgo}Showing the scan from {scannedAgo}.{/if}
        </span>
        {@render copyErrorButton(errorMsg ?? "", "Copy rescan error")}
      </div>
    {/if}

    <div
      class="gp-notice"
      role={headline.tone === "ok" ? "status" : "alert"}
      data-testid="secrets-verdict"
      data-tone={headline.tone}
    >
      {#if headline.tone === "ok"}
        <ShieldCheck size={14} class="shrink-0 mt-0.5" style="color: var(--gp-notice-ink)" />
      {:else}
        <AlertTriangle size={14} class="shrink-0 mt-0.5" style="color: var(--gp-notice-ink)" />
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
            class="whitespace-pre-wrap wrap-break-word font-mono text-micro leading-relaxed select-text opacity-90"
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
          class="gp-chip cursor-pointer"
          data-kind="label"
          data-tone={filter === "all" ? "accent" : "neutral"}
          aria-pressed={filter === "all"}
          onclick={() => (filter = "all")}
        >
          All {findings.length}
        </button>
        {#each chips as chip (chip.location)}
          <button
            type="button"
            class="gp-chip cursor-pointer"
            data-kind="label"
            data-tone={filter === chip.location ? "accent" : chipTone[LOCATION_COPY[chip.location].tone]}
            aria-pressed={filter === chip.location}
            title={LOCATION_COPY[chip.location].hint}
            onclick={() => (filter = filter === chip.location ? "all" : chip.location)}
          >
            {LOCATION_COPY[chip.location].label} {chip.count}
          </button>
        {/each}
      </div>

      <HealthTable caption="Secret findings in the working tree" columns={FINDING_COLUMNS}>
        {#each keyedList(previewRows, findingKey) as { item: finding, key } (key)}
          {@const copy = LOCATION_COPY[finding.location]}
          {@const sameValue = sizes.get(finding.secret_group) ?? 0}
          <tr data-location={finding.location}>
            <td class="px-3 py-1.5">
              <span class="gp-chip" data-kind="label" data-tone={chipTone[copy.tone]} title={copy.hint}>{copy.label}</span>
            </td>
            <td class="px-3 py-1.5 font-mono text-textPrimary break-all">
              <span title={finding.path}>{finding.path}</span>{#if finding.line !== null}<span
                  class="text-textMuted">:{finding.line}</span
                >{/if}
              {#if sameValue > 1}
                <span
                  class="ml-1 font-sans text-micro text-warn whitespace-nowrap"
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
                <div class="font-mono text-micro text-textMuted truncate">{finding.rule_id}</div>
              {/if}
            </td>
            <td class="px-3 py-1.5 text-textMuted capitalize">{finding.confidence || "—"}</td>
            <td class="px-2 py-1.5 text-right">
              {#if finding.location !== "outside"}
                <button
                  type="button"
                  class="gp-icon-btn"
                  data-size="sm"
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
      </HealthTable>
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
      <details class="text-meta text-textMuted px-0.5" data-testid="secrets-scope">
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
