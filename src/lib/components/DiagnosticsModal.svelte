<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import {
    APP_VERSION,
    diagnostics,
    formatDiagnosticReport,
    formatDiagnosticTime,
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
  import { withBackendLogSection, withPersistedLogSection } from "../diagnostics/report";
  import { unreadablePersistedLog, type PersistedLog } from "../diagnostics/types";
  import { formatError } from "../ui/formatError";
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

  async function copyReport() {
    // Backend context is best-effort: the command may not be registered yet,
    // and ANY failure (including IPC rejection) degrades to no section — the
    // clipboard write itself must still succeed.
    let backendLines: string[] = [];
    try {
      backendLines = await invoke<string[]>("cmd_diagnostic_log_tail", {});
    } catch {
      backendLines = [];
    }
    // The durable log is fetched separately and its section is written even
    // on failure: an omitted section reads as a quiet backend, which is the
    // one thing a crash report must never imply when it does not know.
    let persisted: PersistedLog;
    try {
      persisted = await invoke<PersistedLog>("cmd_diagnostic_persisted_log", {});
    } catch (err) {
      persisted = unreadablePersistedLog(formatError(err));
    }
    const report = withPersistedLogSection(
      withBackendLogSection(formatDiagnosticReport($diagnostics), backendLines),
      persisted,
    );
    void copyText(report).then((ok) => {
      if (!ok) {
        copyFailed = true;
        if (copyFailedTimer) clearTimeout(copyFailedTimer);
        copyFailedTimer = setTimeout(() => (copyFailed = false), 2000);
        return;
      }
      copied = true;
      if (copiedTimer) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = false), 1500);
    });
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
      class="w-full max-w-xl gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
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

      <div class="max-h-[60vh] overflow-y-auto p-2 space-y-1">
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
            No diagnostics recorded — errors, warnings and crashes land here.
          </div>
        {/each}
      </div>

      <div class="p-4 border-t border-border/60 bg-surfaceHover/30 flex justify-between gap-2">
        <button type="button" class="gp-btn" onclick={copyReport} disabled={$diagnostics.length === 0}>
          {#if copied}
            <Check size={13} class="text-emerald-400" />
            <span>Copied</span>
          {:else if copyFailed}
            <TriangleAlert size={13} class="text-rose-400" />
            <span class="text-rose-400">Copy failed</span>
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
          >
            <Trash2 size={13} />
            <span>Clear</span>
          </button>
          <button type="button" class="gp-btn" onclick={onClose}>Done</button>
        </div>
      </div>
    </div>
  </div>
{/if}
