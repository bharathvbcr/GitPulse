<script module lang="ts">
  import type { PersistedLog as PersistedLogShape } from "../diagnostics/types";

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

  /** A response belongs to the currently visible opening of the dialog. */
  export function isCurrentBackendLoad(
    generation: number,
    currentGeneration: number,
    open: boolean,
  ): boolean {
    return open && generation === currentGeneration;
  }
</script>

<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import {
    APP_VERSION,
    diagnostics,
    formatDiagnosticReport,
    formatDiagnosticTime,
    redactDiagnosticText,
    staleBuildNote,
    type DiagnosticEntry,
    type DiagnosticSeverity,
  } from "../diagnostics/diagnostics";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { copyText } from "../desktop/clipboard";
  import { invoke } from "@tauri-apps/api/core";
  import {
    formatDiagnosticFailure,
    withBackendLogSection,
    withPersistedLogSection,
  } from "../diagnostics/report";
  import { unreadablePersistedLog, type PersistedLog } from "../diagnostics/types";
  import { TriangleAlert, CircleAlert, ClipboardCopy, Trash2, Activity, Check } from "lucide-svelte";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  type SeverityFilter = "all" | DiagnosticSeverity;

  const FILTERS: Array<{ value: SeverityFilter; label: string }> = [
    { value: "all", label: "All" },
    { value: "error", label: "Errors" },
    { value: "warning", label: "Warnings" },
  ];

  let filter = $state<SeverityFilter>("all");
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let copyFailed = $state(false);
  let copyFailedTimer: ReturnType<typeof setTimeout> | undefined;
  let copying = $state(false);
  let backendStatus = $state<BackendDiagnosticStatus>("loading");
  let backendLines = $state<string[]>([]);
  let persistedLog = $state<PersistedLog | null>(null);
  let memoryReadError = $state<string | null>(null);
  let backendLoadGeneration = 0;
  let backendLoad: Promise<void> | null = null;

  $effect(() => {
    return () => {
      if (copiedTimer) clearTimeout(copiedTimer);
      if (copyFailedTimer) clearTimeout(copyFailedTimer);
    };
  });

  let visibleEntries = $derived(
    ($diagnostics as readonly DiagnosticEntry[]).filter(
      (entry) => filter === "all" || entry.severity === filter,
    ),
  );
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

  async function loadBackendContext(generation: number): Promise<void> {
    const [memoryResult, persistedResult] = await Promise.allSettled([
      invoke<string[]>("cmd_diagnostic_log_tail", {}),
      invoke<PersistedLog>("cmd_diagnostic_persisted_log", {}),
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
    backendStatus = classifyBackendDiagnostics(memoryReadError, backendLines, persistedLog);
  }

  function beginBackendLoad(): Promise<void> {
    const generation = ++backendLoadGeneration;
    backendStatus = "loading";
    backendLines = [];
    persistedLog = null;
    memoryReadError = null;
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
      return;
    }
    void beginBackendLoad();
    return () => {
      backendLoadGeneration += 1;
      backendLoad = null;
    };
  });

  async function copyReport() {
    if (copying) return;
    copying = true;
    let generation = backendLoadGeneration;
    if (!backendLoad && (backendStatus === "idle" || backendStatus === "loading")) {
      const request = beginBackendLoad();
      generation = backendLoadGeneration;
      await request;
    } else if (backendLoad) {
      await backendLoad;
    }
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) {
      copying = false;
      return;
    }

    let report = withBackendLogSection(formatDiagnosticReport($diagnostics), backendLines);
    if (memoryReadError) {
      report = [
        report,
        "",
        "Backend memory log — unavailable",
        `  ! could not be read: ${memoryReadError}`,
      ].join("\n");
    }
    report = withPersistedLogSection(
      report,
      persistedLog ?? unreadablePersistedLog("backend diagnostics have not loaded"),
    );
    const ok = await copyText(report);
    copying = false;
    if (!isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)) return;
    if (!ok) {
      copyFailed = true;
      if (copyFailedTimer) clearTimeout(copyFailedTimer);
      copyFailedTimer = setTimeout(() => (copyFailed = false), 2000);
      return;
    }
    copied = true;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied = false), 1500);
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
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-xl max-h-[calc(100vh-2rem)] min-h-0 gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between gap-3">
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
                <span class="ml-1 px-1 rounded-full bg-rose-500/20 text-rose-400 text-[10px] font-semibold">{errorCount}</span>
              {:else if option.value === "warning" && warningCount > 0}
                <span class="ml-1 px-1 rounded-full bg-amber-500/20 text-amber-400 text-[10px] font-semibold">{warningCount}</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>

      <div class="min-h-0 flex-1 overflow-y-auto p-2 space-y-2">
        <section
          aria-label="Backend diagnostics"
          class="rounded-xl border border-border/70 bg-background px-3 py-2 space-y-2"
        >
          <div class="flex items-center gap-2">
            <span
              class="h-2 w-2 rounded-full shrink-0 {backendStatus === 'healthy'
                ? 'bg-emerald-400'
                : backendStatus === 'degraded' || backendStatus === 'empty'
                  ? 'bg-amber-400'
                  : backendStatus === 'unavailable'
                    ? 'bg-rose-400'
                    : 'bg-textMuted'}"
            ></span>
            <span class="font-medium text-textPrimary" role="status" aria-live="polite">{backendStatusLabel(backendStatus)}</span>
            {#if backendStatus === "degraded" || backendStatus === "unavailable"}
              <button
                type="button"
                class="gp-btn !py-0.5 !px-2 ml-auto"
                onclick={() => void beginBackendLoad()}
              >Retry</button>
            {/if}
          </div>

          {#if memoryReadError}
            <p class="text-[11px] text-amber-300 break-words">Current-session log could not be read: {memoryReadError}</p>
          {/if}
          {#if persistedLog?.degraded}
            <p class="text-[11px] text-amber-300 break-words">Durable log is incomplete: {persistedLog.degraded}</p>
          {/if}
          {#if persistedLog?.path}
            <p class="text-[10px] text-textMuted font-mono break-all">Durable log: {persistedLog.path}</p>
          {/if}

          {#if backendLines.length > 0}
            <details open={$diagnostics.length === 0}>
              <summary class="cursor-pointer text-[11px] text-textMuted">Current session ({backendLines.length} lines)</summary>
              <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words font-mono text-[10px] leading-relaxed text-textPrimary select-text">{backendLines.join("\n")}</pre>
            </details>
          {/if}
          {#if persistedLog && persistedLog.lines.length > 0}
            <details open={$diagnostics.length === 0}>
              <summary class="cursor-pointer text-[11px] text-textMuted">Durable history ({persistedLog.lines.length} lines)</summary>
              <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words font-mono text-[10px] leading-relaxed text-textPrimary select-text">{persistedLog.lines.join("\n")}</pre>
            </details>
          {/if}
        </section>

        <div class="px-1 pt-1 text-[10px] font-semibold uppercase tracking-wider text-textMuted">
          Frontend diagnostics
        </div>
        {#each visibleEntries as entry (entry.id)}
          <div
            class="rounded-xl border px-3 py-2 {entry.severity === 'error'
              ? 'border-rose-500/25 bg-rose-500/5'
              : 'border-amber-500/25 bg-amber-500/5'}"
          >
            <div class="flex items-center gap-2 mb-1">
              {#if entry.severity === "error"}
                <CircleAlert size={12} class="text-rose-400 shrink-0" />
              {:else}
                <TriangleAlert size={12} class="text-amber-400 shrink-0" />
              {/if}
              <span
                class="text-[10px] font-semibold uppercase tracking-wider {entry.severity === 'error'
                  ? 'text-rose-400'
                  : 'text-amber-400'}"
              >{entry.severity}</span>
              <span class="px-1.5 py-px rounded-md bg-surfaceHover text-[10px] text-textMuted font-mono">{entry.source}</span>
              <!-- Only entries that did not come from the running build are
                   marked, so the badge means something when it appears: the
                   ring is persisted, so a log can outlive the build that
                   wrote it and describe a bug that is already fixed. -->
              {#if staleBuildNote(entry.version, APP_VERSION)}
                <span
                  class="px-1.5 py-px rounded-md bg-amber-500/15 text-[10px] text-amber-400/90 font-mono shrink-0"
                  title="Recorded by {entry.version ?? 'an earlier build'}; this app is running {APP_VERSION}."
                >{entry.version ?? "older build"}</span>
              {/if}
              <span class="text-[10px] text-textMuted ml-auto font-mono shrink-0">{formatDiagnosticTime(entry.at)}</span>
              <!-- Repeats are grouped by fingerprint, so occurrences can
                   differ in per-run detail. Saying so keeps the counter from
                   reading as N verbatim copies of the text below it. -->
              {#if entry.count > 1}
                <span
                  class="px-1.5 py-px rounded-full bg-surfaceHover text-[10px] font-semibold text-textPrimary shrink-0"
                  title={entry.varied
                    ? `${entry.count} occurrences; they were not identical — the most recent is shown.`
                    : `${entry.count} identical occurrences.`}
                >×{entry.count}{entry.varied ? " differing" : ""}</span>
              {/if}
            </div>
            <pre class="whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-textPrimary m-0">{entry.message}</pre>
          </div>
        {:else}
          <div class="py-10 text-center text-textMuted" role="status">
            No frontend diagnostics recorded.
          </div>
        {/each}
      </div>

      <div class="p-3 border-t border-border/60 bg-surfaceHover/30 shrink-0 space-y-2">
        <p class="text-[10px] leading-relaxed text-textMuted">
          Review local paths and command output before sharing. Copy only places the report on your
          clipboard; it does not upload or delete any logs.
        </p>
        <div class="flex justify-between gap-2">
          <button type="button" class="gp-btn" onclick={copyReport} disabled={copying}>
            {#if copied}
              <Check size={13} class="text-emerald-400" />
              <span>Copied</span>
            {:else if copyFailed}
              <TriangleAlert size={13} class="text-rose-400" />
              <span class="text-rose-400">Copy failed</span>
            {:else if copying}
              <Activity size={13} class="animate-spin" />
              <span>Preparing…</span>
            {:else}
              <ClipboardCopy size={13} />
              <span>Copy Report</span>
            {/if}
          </button>
          <div class="flex gap-2">
            <button
              type="button"
              class="gp-btn"
              onclick={() => diagnostics.clear()}
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
