<script module lang="ts">
  import type { DaemonStatus, Degraded, PersistedLog as PersistedLogShape } from "../diagnostics/types";
  import type { DiagnosticEntry, DiagnosticGroup, DiagnosticSeverity } from "../diagnostics/diagnostics";

  export type BackendDiagnosticStatus =
    | "idle"
    | "loading"
    | "healthy"
    | "degraded"
    | "unavailable"
    | "empty";

  /**
   * Classifies the two independent backend readers without letting a partial
   * answer masquerade as a healthy empty log.
   */
  export function classifyBackendDiagnostics(
    memoryReadError: string | null,
    memoryLines: readonly string[],
    persisted: PersistedLogShape,
  ): BackendDiagnosticStatus {
    const hasLines = memoryLines.length > 0 || persisted.lines.length > 0;
    if (memoryReadError && !persisted.path && !hasLines) return "unavailable";
    if (memoryReadError || persisted.degraded || !persisted.path) return "degraded";
    return hasLines ? "healthy" : "empty";
  }

  export type SeverityFilter = "all" | DiagnosticSeverity;

  /**
   * What the list says when the active filter leaves it empty, or null when the
   * ring itself is empty. Entries that exist but are filtered out must never
   * read as "nothing was recorded": someone debugging a failure would conclude
   * the app logged nothing.
   */
  export function hiddenByFilterNotice(filter: SeverityFilter, total: number): string | null {
    if (filter === "all" || total <= 0) return null;
    const wanted = filter === "error" ? "errors" : "warnings";
    return `No ${wanted} recorded. ${total} other ${total === 1 ? "entry" : "entries"} hidden by this filter.`;
  }

  /** A group key as a DOM id fragment: the key holds NULs and message text. */
  export function groupDomId(key: string): string {
    let hash = 5381;
    for (let i = 0; i < key.length; i += 1) hash = ((hash * 33) ^ key.charCodeAt(i)) >>> 0;
    return hash.toString(36);
  }

  /** A response belongs to the currently visible opening of the dialog. */
  export function isCurrentBackendLoad(
    generation: number,
    currentGeneration: number,
    open: boolean,
  ): boolean {
    return open && generation === currentGeneration;
  }

  /** Collects entries from groups whose keys are selected, in display order. */
  export function selectedDiagnosticEntries(
    groups: readonly DiagnosticGroup[],
    selectedKeys: Readonly<Record<string, boolean>>,
  ): DiagnosticEntry[] {
    return groups.flatMap((group) => (selectedKeys[group.key] === true ? group.entries : []));
  }
</script>

<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import {
    APP_VERSION,
    diagnostics,
    diagnosticPersistenceLabel,
    diagnosticEntryFirstAt,
    formatDiagnosticRange,
    formatDiagnosticReport,
    groupDiagnostics,
    redactDiagnosticText,
    staleBuildNote,
  } from "../diagnostics/diagnostics";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { scrollEdgeFade } from "../ui/scrollEdgeFade";
  import { LAYERS } from "../ui/layers";
  import { copyText } from "../desktop/clipboard";
  import { invoke } from "../ipc/invoke";
  import {
    formatDiagnosticFailure,
    withBackendLogSection,
    withPersistedLogSection,
  } from "../diagnostics/report";
  import { unreadablePersistedLog, type PersistedLog } from "../diagnostics/types";
  import Checkbox from "./Checkbox.svelte";
  import { TriangleAlert, CircleAlert, ChevronDown, ClipboardCopy, Copy, Trash2, Activity, Check } from "@lucide/svelte";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  const FILTERS: Array<{ value: SeverityFilter; label: string }> = [
    { value: "all", label: "All" },
    { value: "error", label: "Errors" },
    { value: "warning", label: "Warnings" },
  ];

  type CopyTarget = "report" | "selected";

  let filter = $state<SeverityFilter>("all");
  let copied = $state<CopyTarget | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let copyFailed = $state<CopyTarget | null>(null);
  let copyFailedTimer: ReturnType<typeof setTimeout> | undefined;
  let copying = $state<CopyTarget | null>(null);
  let copiedGroupKey = $state<string | null>(null);
  let groupCopiedTimer: ReturnType<typeof setTimeout> | undefined;
  let copyFailedGroupKey = $state<string | null>(null);
  let groupCopyFailedTimer: ReturnType<typeof setTimeout> | undefined;
  let backendStatus = $state<BackendDiagnosticStatus>("loading");
  let backendLines = $state<string[]>([]);
  let persistedLog = $state<PersistedLog | null>(null);
  let memoryReadError = $state<string | null>(null);
  let daemonNotice = $state<string | null>(null);
  let degradedSubsystems = $state<{ subsystem: string; reason: string; panics: number; stopped: boolean }[]>([]);
  let backendLoadGeneration = 0;
  let backendLoad: Promise<void> | null = null;
  const diagnosticHealth = diagnostics.health;

  $effect(() => {
    return () => {
      if (copiedTimer) clearTimeout(copiedTimer);
      if (copyFailedTimer) clearTimeout(copyFailedTimer);
      if (groupCopiedTimer) clearTimeout(groupCopiedTimer);
      if (groupCopyFailedTimer) clearTimeout(groupCopyFailedTimer);
    };
  });

  let visibleGroups = $derived(
    groupDiagnostics(
      ($diagnostics as readonly DiagnosticEntry[]).filter(
        (entry) => filter === "all" || entry.severity === filter,
      ),
    ),
  );
  /** Which groups have their occurrence list open, by group key. */
  let expanded = $state<Record<string, boolean>>({});
  /** Which visible groups are checked for selective copying, by group key. */
  let selected = $state<Record<string, boolean>>({});
  let selectedGroups = $derived(visibleGroups.filter((group) => selected[group.key] === true));
  let selectedEntries = $derived(selectedDiagnosticEntries(visibleGroups, selected));
  let allVisibleSelected = $derived(
    visibleGroups.length > 0 && selectedGroups.length === visibleGroups.length,
  );
  let someVisibleSelected = $derived(
    selectedGroups.length > 0 && selectedGroups.length < visibleGroups.length,
  );
  let hiddenNotice = $derived(hiddenByFilterNotice(filter, $diagnostics.length));
  let errorCount = $derived(
    ($diagnostics as readonly DiagnosticEntry[]).reduce(
      (total, entry) => (entry.severity === "error" ? total + entry.count : total),
      0,
    ),
  );
  let warningCount = $derived(
    ($diagnostics as readonly DiagnosticEntry[]).reduce(
      (total, entry) => (entry.severity === "warning" ? total + entry.count : total),
      0,
    ),
  );

  function backendStatusLabel(status: BackendDiagnosticStatus): string {
    switch (status) {
      case "loading":
        return "Loading backend diagnostics…";
      case "healthy":
        return "Backend diagnostics healthy";
      case "degraded":
        return "Backend diagnostics degraded";
      case "unavailable":
        return "Backend diagnostics unavailable";
      case "empty":
        return "Backend diagnostics empty";
      default:
        return "Backend diagnostics not loaded";
    }
  }

  /** What to say about the attribution daemon, or null when there is nothing to say. */
  function daemonNoticeFor(status: DaemonStatus): string | null {
    if (status.error) return `The attribution daemon couldn't be checked: ${redactDiagnosticText(String(status.error))}`;
    if (!status.running || !status.mismatch) return null;
    return `A gitpulsed from another version (${redactDiagnosticText(String(status.version ?? "unknown"))}, this app is ${redactDiagnosticText(String(status.app_version))}) is running${status.pid ? ` as pid ${status.pid}` : ""}. Restart it so it writes with this version's rules.`;
  }

  async function loadBackendContext(generation: number): Promise<void> {
    const [memoryResult, persistedResult, degradedResult, daemonResult] = await Promise.allSettled([
      invoke<string[]>("cmd_diagnostic_log_tail", {}),
      invoke<PersistedLog>("cmd_diagnostic_persisted_log", {}),
      invoke<Degraded[]>("cmd_degraded_subsystems", {}),
      invoke<DaemonStatus>("cmd_daemon_status", {}),
    ]);
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) return;

    backendLines =
      memoryResult.status === "fulfilled"
        ? memoryResult.value.map(redactDiagnosticText)
        : [];
    memoryReadError = memoryResult.status === "rejected"
      ? formatDiagnosticFailure(memoryResult.reason)
      : null;
    persistedLog =
      persistedResult.status === "fulfilled"
        ? {
            path: redactDiagnosticText(persistedResult.value.path),
            lines: persistedResult.value.lines.map(redactDiagnosticText),
            degraded: persistedResult.value.degraded
              ? redactDiagnosticText(persistedResult.value.degraded)
              : null,
          }
        : unreadablePersistedLog(formatDiagnosticFailure(persistedResult.reason));
    // A background thread that panicked and is being retried. Best effort: an
    // unreadable list is not a claim that nothing is degraded, but it must not
    // hide the logs either.
    degradedSubsystems =
      degradedResult.status === "fulfilled" && Array.isArray(degradedResult.value)
        ? degradedResult.value.map((d) => ({
            subsystem: redactDiagnosticText(String(d.subsystem)),
            reason: redactDiagnosticText(String(d.reason)),
            panics: Number(d.panics) || 1,
            stopped: d.stopped === true,
          }))
        : [];
    daemonNotice =
      daemonResult.status === "fulfilled" ? daemonNoticeFor(daemonResult.value) : null;
    backendStatus = classifyBackendDiagnostics(memoryReadError, backendLines, persistedLog);
  }

  function beginBackendLoad(): Promise<void> {
    const generation = ++backendLoadGeneration;
    backendStatus = "loading";
    backendLines = [];
    persistedLog = null;
    memoryReadError = null;
    degradedSubsystems = [];
    daemonNotice = null;
    const request = loadBackendContext(generation);
    backendLoad = request;
    void request.finally(() => {
      if (generation === backendLoadGeneration && backendLoad === request) {
        backendLoad = null;
      }
    });
    return request;
  }

  // Load while the modal is open, not when Copy is pressed. The generation is
  // invalidated on close/destroy so a slow response from an earlier opening
  // cannot overwrite a newer one.
  $effect(() => {
    if (!isOpen) {
      backendStatus = "idle";
      backendLines = [];
      persistedLog = null;
      memoryReadError = null;
      backendLoad = null;
      selected = {};
      copied = null;
      copyFailed = null;
      copying = null;
      copiedGroupKey = null;
      copyFailedGroupKey = null;
      return;
    }
    void beginBackendLoad();
    return () => {
      backendLoadGeneration += 1;
      backendLoad = null;
    };
  });

  function toggleGroupSelection(key: string, next: boolean) {
    if (next) {
      selected = { ...selected, [key]: true };
      return;
    }
    const { [key]: _, ...rest } = selected;
    selected = rest;
  }

  function toggleSelectAllVisible(next: boolean) {
    const nextSelected = { ...selected };
    for (const group of visibleGroups) {
      if (next) nextSelected[group.key] = true;
      else delete nextSelected[group.key];
    }
    selected = nextSelected;
  }

  function clearSelection() {
    selected = {};
  }

  function clearFrontend() {
    diagnostics.clear();
    selected = {};
  }

  function flashCopyOutcome(target: CopyTarget, ok: boolean) {
    if (!ok) {
      copyFailed = target;
      if (copyFailedTimer) clearTimeout(copyFailedTimer);
      copyFailedTimer = setTimeout(() => {
        if (copyFailed === target) copyFailed = null;
      }, 2000);
      return;
    }
    copied = target;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => {
      if (copied === target) copied = null;
    }, 1500);
  }

  async function copySelected() {
    if (copying !== null || selectedEntries.length === 0) return;
    const entriesToCopy = selectedEntries;
    const generation = backendLoadGeneration;
    copying = "selected";
    const report = formatDiagnosticReport(entriesToCopy, new Date(), APP_VERSION);
    const ok = await copyText(report);
    copying = null;
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) return;
    flashCopyOutcome("selected", ok);
  }

  async function copyGroup(group: DiagnosticGroup) {
    const generation = backendLoadGeneration;
    const report = formatDiagnosticReport(group.entries, new Date(), APP_VERSION);
    const ok = await copyText(report);
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) return;
    if (!ok) {
      copyFailedGroupKey = group.key;
      if (groupCopyFailedTimer) clearTimeout(groupCopyFailedTimer);
      groupCopyFailedTimer = setTimeout(() => {
        if (copyFailedGroupKey === group.key) copyFailedGroupKey = null;
      }, 2000);
      return;
    }
    copiedGroupKey = group.key;
    if (groupCopiedTimer) clearTimeout(groupCopiedTimer);
    groupCopiedTimer = setTimeout(() => {
      if (copiedGroupKey === group.key) copiedGroupKey = null;
    }, 1500);
  }

  async function copyReport() {
    if (copying !== null) return;
    copying = "report";
    let generation = backendLoadGeneration;
    if (!backendLoad && (backendStatus === "idle" || backendStatus === "loading")) {
      const request = beginBackendLoad();
      generation = backendLoadGeneration;
      await request;
    } else if (backendLoad) {
      await backendLoad;
    }
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) {
      copying = null;
      return;
    }

    let report = withBackendLogSection(formatDiagnosticReport($diagnostics, new Date(), APP_VERSION, $diagnosticHealth), backendLines);
    if (memoryReadError) {
      report = [
        report,
        "",
        "Backend memory log — unavailable",
        `  ! couldn't be read: ${memoryReadError}`,
      ].join("\n");
    }
    report = withPersistedLogSection(
      report,
      persistedLog ?? unreadablePersistedLog("backend diagnostics have not loaded"),
    );
    const ok = await copyText(report);
    copying = null;
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) return;
    flashCopyOutcome("report", ok);
  }
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    aria-label="Diagnostics"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && onClose?.()}
    onkeydown={(e) => e.key === "Escape" && onClose?.()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="gp-scrim bg-black/40 flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-4xl max-h-[calc(100vh-2rem)] min-h-0 gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 gp-section-edge flex items-center justify-between gap-3">
        <div class="flex items-center gap-2 text-sm font-semibold text-textPrimary shrink-0">
          <Activity size={16} class="text-accent" />
          <span>Diagnostics</span>
        </div>
        <div class="gp-segmented" role="group" aria-label="Filter by severity">
          {#each FILTERS as option (option.value)}
            <button
              type="button"
              onclick={() => (filter = option.value)}
              aria-pressed={filter === option.value}
              data-active={filter === option.value ? "true" : "false"}
              class="gp-seg-btn"
              title="Show {option.label.toLowerCase()} only"
            >
              <span>{option.label}</span>
              {#if option.value === "error" && errorCount > 0}
                <span class="ml-1 px-1 rounded-full bg-danger/20 text-danger text-micro font-semibold">{errorCount}</span>
              {:else if option.value === "warning" && warningCount > 0}
                <span class="ml-1 px-1 rounded-full bg-warn/20 text-warn text-micro font-semibold">{warningCount}</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto p-2 space-y-2 scroll-py-8" data-dialog-body use:scrollEdgeFade>
        <section
          aria-label="Backend diagnostics"
          class="rounded-xl border border-edge bg-background px-3 py-2 space-y-2"
        >
          <div class="flex items-center gap-2">
            <span
              class="h-2 w-2 rounded-full shrink-0 {backendStatus === 'healthy'
                ? 'bg-ok'
                : backendStatus === 'degraded' || backendStatus === 'empty'
                  ? 'bg-warn'
                  : backendStatus === 'unavailable'
                    ? 'bg-danger'
                    : 'bg-textMuted'}"
            ></span>
            <span class="font-medium text-textPrimary" role="status" aria-live="polite">{backendStatusLabel(backendStatus)}</span>
            {#if backendStatus === "degraded" || backendStatus === "unavailable"}
              <button
                type="button"
                class="gp-btn px-2! ml-auto" data-size="sm"
                onclick={() => void beginBackendLoad()}
              >Try again</button>
            {/if}
          </div>

          <div class="pt-2 space-y-1">
            <div class="flex items-center gap-2" role="status" aria-live="polite">
              <span>{diagnosticPersistenceLabel($diagnosticHealth)}</span>
              {#if $diagnosticHealth.persistence === "memory-only"}
                <button type="button" class="gp-btn px-2! ml-auto" data-size="sm" onclick={() => diagnostics.retryPersistence()}>Try saving again</button>
              {/if}
            </div>
            {#if $diagnosticHealth.persistenceError}
              <p class="text-meta text-warn wrap-break-word">{$diagnosticHealth.persistenceError}</p>
            {/if}
            {#if $diagnosticHealth.restorationError}
              <p class="text-meta text-warn wrap-break-word">Saved history incomplete: {$diagnosticHealth.restorationError}</p>
            {/if}
            {#if $diagnosticHealth.suppressedRuntimeEvents > 0}
              <p class="text-meta text-textMuted">{$diagnosticHealth.suppressedRuntimeEvents} runtime notifications suppressed this session.</p>
            {/if}
          </div>

          {#if memoryReadError}
            <p class="text-meta text-warn wrap-break-word">Current-session log couldn't be read: {memoryReadError}</p>
          {/if}
          {#if daemonNotice}
            <p class="text-meta text-warn wrap-break-word">{daemonNotice}</p>
          {/if}
          {#each degradedSubsystems as degraded (degraded.subsystem)}
            <p class="text-meta text-warn wrap-break-word">
              {degraded.subsystem} hit an internal error{degraded.panics > 1 ? ` ${degraded.panics} times` : ""} and {degraded.stopped ? "has stopped; restart Waylode to bring it back" : "is retrying"}: {degraded.reason}
            </p>
          {/each}
          {#if persistedLog?.degraded}
            <p class="text-meta text-warn wrap-break-word">Durable log is incomplete: {persistedLog.degraded}</p>
          {/if}
          {#if persistedLog?.path}
            <p class="text-micro text-textMuted font-mono break-all">Durable log: {persistedLog.path}</p>
          {/if}

          {#if backendLines.length > 0}
            <details open={$diagnostics.length === 0}>
              <summary class="cursor-pointer text-meta text-textMuted">Current session ({backendLines.length} lines)</summary>
              <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap wrap-break-word font-mono text-micro leading-relaxed text-textPrimary select-text">{backendLines.join("\n")}</pre>
            </details>
          {/if}
          {#if persistedLog && persistedLog.lines.length > 0}
            <details open={$diagnostics.length === 0}>
              <summary class="cursor-pointer text-meta text-textMuted">Durable history ({persistedLog.lines.length} lines)</summary>
              <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap wrap-break-word font-mono text-micro leading-relaxed text-textPrimary select-text">{persistedLog.lines.join("\n")}</pre>
            </details>
          {/if}
        </section>

        <div class="px-1 pt-1 flex items-center justify-between gap-2">
          <div class="flex items-center gap-2">
            {#if visibleGroups.length > 0}
              <Checkbox
                ariaLabel="Select all visible diagnostics"
                checked={allVisibleSelected}
                indeterminate={someVisibleSelected}
                onchange={toggleSelectAllVisible}
              />
            {/if}
            <span class="gp-label">Frontend diagnostics</span>
          </div>
          {#if selectedGroups.length > 0}
            <div class="flex items-center gap-2">
              <span class="text-micro text-textMuted">{selectedGroups.length} selected</span>
              <button
                type="button"
                class="gp-btn"
                data-size="sm"
                onclick={clearSelection}
              >Clear selection</button>
            </div>
          {/if}
        </div>
        {#each visibleGroups as group (group.key)}
          {@const open = expanded[group.key] === true}
          {@const isSelected = selected[group.key] === true}
          {@const tone = group.severity === "error" ? "text-danger" : "text-warn"}
          <div
            class="rounded-xl border px-3 py-2 {group.severity === 'error'
              ? 'border-danger/35 bg-danger/5'
              : 'border-warn/35 bg-warn/5'}"
          >
            {#snippet head()}
              {#if group.severity === "error"}
                <CircleAlert size={12} class="text-danger shrink-0" />
              {:else}
                <TriangleAlert size={12} class="text-warn shrink-0" />
              {/if}
              <span class="text-micro font-semibold {tone}">{group.severity}</span>
              <span class="px-1.5 py-px rounded-sm bg-surfaceHover text-micro text-textMuted font-mono">{group.source}</span>
              <!-- Only entries that did not come from the running build are
                   marked, so the badge means something when it appears: the
                   ring is persisted, so a log can outlive the build that
                   wrote it and describe a bug that is already fixed. -->
              {#if staleBuildNote(group.version, APP_VERSION)}
                <span
                  class="px-1.5 py-px rounded-sm bg-warn/15 text-micro text-warn/90 font-mono shrink-0"
                  title="Recorded by {group.version ?? 'an earlier build'}; this app is running {APP_VERSION}."
                >{group.version ?? "older build"}</span>
              {/if}
              <span class="text-micro text-textMuted ml-auto font-mono shrink-0">{formatDiagnosticRange(group.firstAt, group.lastAt)}</span>
              <!-- Occurrences are grouped by normalised message, so they can
                   differ in per-run detail. Saying so keeps the counter from
                   reading as N verbatim copies of the text below it. -->
              {#if group.count > 1}
                <span
                  class="px-1.5 py-px rounded-full bg-surfaceHover text-micro font-semibold text-textPrimary shrink-0"
                  title={group.varied
                    ? `${group.count} occurrences; they were not identical — the most recent is shown.`
                    : `${group.count} identical occurrences.`}
                >×{group.count}{group.varied ? " differing" : ""}</span>
              {/if}
            {/snippet}
            <div class="flex items-center gap-2 mb-1">
              <Checkbox
                ariaLabel="Select {group.severity} from {group.source}"
                checked={isSelected}
                onchange={(next) => toggleGroupSelection(group.key, next)}
              />
              {#if group.count > 1}
                <button
                  type="button"
                  class="flex flex-1 min-w-0 items-center gap-2 text-left cursor-pointer"
                  aria-expanded={open}
                  aria-controls={`diagnostic-occurrences-${groupDomId(group.key)}`}
                  title={open ? "Hide each occurrence" : "Show each occurrence"}
                  onclick={() => (expanded = { ...expanded, [group.key]: !open })}
                >
                  {@render head()}
                  <ChevronDown size={12} class="shrink-0 text-textMuted transition-transform {open ? 'rotate-180' : ''}" />
                </button>
              {:else}
                <div class="flex flex-1 min-w-0 items-center gap-2">
                  {@render head()}
                </div>
              {/if}
              <button
                type="button"
                class="gp-icon-btn shrink-0"
                data-size="sm"
                aria-label={copiedGroupKey === group.key
                  ? `Copied ${group.severity} from ${group.source}`
                  : `Copy ${group.severity} from ${group.source}`}
                title={copiedGroupKey === group.key
                  ? "Copied"
                  : copyFailedGroupKey === group.key
                    ? "Copy failed"
                    : "Copy only this diagnostic"}
                onclick={() => void copyGroup(group)}
              >
                {#if copiedGroupKey === group.key}
                  <Check size={12} class="text-ok" />
                {:else if copyFailedGroupKey === group.key}
                  <TriangleAlert size={12} class="text-danger" />
                {:else}
                  <Copy size={12} />
                {/if}
              </button>
            </div>
            <pre class="whitespace-pre-wrap wrap-break-word font-mono text-meta leading-relaxed text-textPrimary m-0 select-text">{group.message}</pre>
            {#if open}
              <ul
                id={`diagnostic-occurrences-${groupDomId(group.key)}`}
                aria-label="Occurrences"
                class="mt-2 space-y-1 rounded-lg bg-background/60 px-2 py-1.5 list-none"
              >
                {#each group.entries as entry (entry.id)}
                  <li class="text-micro text-textMuted">
                    <span class="font-mono">{formatDiagnosticRange(diagnosticEntryFirstAt(entry), entry.at)}</span>
                    {#if entry.count > 1}<span class="font-semibold text-textPrimary"> ×{entry.count}</span>{/if}
                    {#if entry.message !== group.message}
                      <pre class="mt-0.5 whitespace-pre-wrap wrap-break-word font-mono text-meta leading-relaxed text-textPrimary m-0 select-text">{entry.message}</pre>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        {:else}
          <div class="py-10 text-center text-textMuted flex flex-col items-center gap-3" role="status">
            {#if hiddenNotice}
              <span>{hiddenNotice}</span>
              <button
                type="button"
                class="gp-btn px-2.5! text-meta!" data-size="sm"
                onclick={() => (filter = "all")}
              >
                Show all
              </button>
            {:else}
              <span>No frontend diagnostics recorded.</span>
            {/if}
          </div>
        {/each}
      </div>

      {#snippet copyButtonContent(target: CopyTarget, idleLabel: string)}
        {#if copied === target}
          <Check size={13} class="text-ok" />
          <span>Copied</span>
        {:else if copyFailed === target}
          <TriangleAlert size={13} class="text-danger" />
          <span class="text-danger">Copy failed</span>
        {:else if copying === target}
          <Activity size={13} class="animate-spin" />
          <span>Preparing…</span>
        {:else}
          <ClipboardCopy size={13} />
          <span>{idleLabel}</span>
        {/if}
      {/snippet}

      <div class="p-3 gp-section-edge bg-surfaceHover/30 shrink-0 space-y-2">
        <p class="text-micro leading-relaxed text-textMuted">
          Review local paths and command output before sharing. Copy only places the report on your
          clipboard; it does not upload or delete any logs.
        </p>
        <div class="flex justify-between gap-2">
          <div class="flex items-center gap-2">
            {#if selectedGroups.length > 0}
              <button type="button" class="gp-btn" data-variant="primary" onclick={copySelected} disabled={copying !== null}>
                {@render copyButtonContent("selected", `Copy selected (${selectedGroups.length})`)}
              </button>
              <button type="button" class="gp-btn" onclick={copyReport} disabled={copying !== null}>
                {@render copyButtonContent("report", "Copy full report")}
              </button>
            {:else}
              <button type="button" class="gp-btn" onclick={copyReport} disabled={copying !== null}>
                {@render copyButtonContent("report", "Copy report")}
              </button>
            {/if}
          </div>
          <div class="flex gap-2">
            <button
              type="button"
              class="gp-btn"
              onclick={clearFrontend}
              disabled={$diagnostics.length === 0}
              title="Clear only the frontend diagnostics shown in this app; backend logs remain on disk"
            >
              <Trash2 size={13} />
              <span>Clear frontend</span>
            </button>
            <button type="button" class="gp-btn" onclick={onClose}>Done</button>
          </div>
        </div>
      </div>
    </div>
  </div>
{/if}
