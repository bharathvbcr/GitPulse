<script lang="ts">
  /**
   * End-of-session work summary card rendered at the bottom of the timeline.
   * Uses the shared clipboard seam (`createCopyFlash`) and `MarkdownBody`.
   */
  import { onDestroy } from "svelte";
  import { Check, Copy, LoaderCircle, RotateCcw, Sparkles, TriangleAlert, X } from "@lucide/svelte";
  import MarkdownBody from "../MarkdownBody.svelte";
  import { createCopyFlash } from "../../desktop/clipboard";
  import type { ThreadWorkSummary } from "../../mavi/types";

  let {
    summary,
    loading,
    error,
    onRegenerate,
    onDismiss,
  }: {
    summary: ThreadWorkSummary | null;
    loading: boolean;
    error: string | null;
    onRegenerate: () => void;
    onDismiss: () => void;
  } = $props();

  let copied = $state(false);
  const summaryCopy = createCopyFlash((value) => (copied = value), 1600);
  onDestroy(summaryCopy.dispose);

  async function copySummary() {
    if (!summary?.text) return;
    await summaryCopy.copy(summary.text);
  }
</script>

{#if loading || error || summary}
  <section class="mavi-summary gp-card" data-testid="mavi-session-summary" aria-label="Session summary">
    <header class="mavi-summary-head flex items-center gap-2">
      <Sparkles size={13} class="text-accent shrink-0" aria-hidden="true" />
      <span class="text-xs font-medium text-textPrimary">Session summary</span>
      {#if summary?.model}
        <span class="gp-chip shrink-0" data-kind="label" data-tone="accent">{summary.model}</span>
      {/if}
      <span class="ml-auto"></span>
      {#if summary?.text}
        <button
          type="button"
          class="gp-icon-btn"
          data-size="sm"
          aria-label={copied ? "Copied summary" : "Copy summary"}
          title={copied ? "Copied" : "Copy summary"}
          onclick={copySummary}
        >
          {#if copied}
            <Check size={12} class="text-ok" aria-hidden="true" />
          {:else}
            <Copy size={12} aria-hidden="true" />
          {/if}
        </button>
      {/if}
      <button
        type="button"
        class="gp-icon-btn"
        data-size="sm"
        disabled={loading}
        aria-label="Regenerate summary"
        title="Regenerate summary"
        onclick={onRegenerate}
      >
        <RotateCcw size={12} aria-hidden="true" />
      </button>
      <button
        type="button"
        class="gp-icon-btn"
        data-size="sm"
        aria-label="Dismiss summary"
        title="Dismiss summary"
        onclick={onDismiss}
      >
        <X size={12} aria-hidden="true" />
      </button>
    </header>

    <div class="mavi-summary-body">
      {#if loading}
        <div class="flex items-center gap-2 text-meta text-textMuted" role="status">
          <LoaderCircle size={13} class="text-accent shrink-0 animate-spin" aria-hidden="true" />
          <span>Summarizing session work with a small model…</span>
        </div>
      {:else if error}
        <div class="gp-notice flex items-center gap-2" data-tone="danger" role="alert">
          <TriangleAlert size={13} class="text-danger shrink-0" aria-hidden="true" />
          <span class="text-meta flex-1 min-w-0">{error}</span>
          <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={onRegenerate}>Try again</button>
        </div>
      {:else if summary}
        <MarkdownBody source={summary.text} class="text-xs text-textPrimary select-text" />
        {#if summary.truncated || summary.warnings?.length}
          <div class="mt-2 flex flex-col gap-0.5 text-micro text-textMuted">
            {#if summary.truncated}
              <span>Digest was trimmed to fit the model limit.</span>
            {/if}
            {#each summary.warnings ?? [] as warning (warning)}
              <span>{warning}</span>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  </section>
{/if}

<style>
  .mavi-summary {
    border-color: rgb(var(--c-accent) / 0.35);
    overflow: hidden;
  }
  .mavi-summary-head {
    padding: 8px 10px 8px 14px;
    background: rgb(var(--c-accent) / 0.06);
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-summary-body {
    padding: 12px 14px;
  }
</style>
