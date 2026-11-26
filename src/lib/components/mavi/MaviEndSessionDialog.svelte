<script lang="ts">
  /**
   * Confirmation dialog when ending a Mavi thread's agent, with an option to
   * generate a concise Markdown summary of the session's work.
   */
  import { untrack } from "svelte";
  import { Sparkles, Square } from "@lucide/svelte";
  import { maviPrefs, type MaviSummaryEngine } from "../../mavi/prefs.svelte";
  import Dialog from "../Dialog.svelte";
  import Checkbox from "../Checkbox.svelte";

  const CLAUDE_SUMMARY_MODELS = ["haiku", "claude-haiku-5-5", "sonnet"] as const;

  let {
    open = $bindable(false),
    title,
    working,
    hasActivity,
    onConfirm,
    onclose,
  }: {
    open?: boolean;
    title: string;
    working: boolean;
    hasActivity: boolean;
    onConfirm: (options: { summarize: boolean; engine: MaviSummaryEngine; model: string }) => void;
    onclose?: () => void;
  } = $props();

  let summarize = $state(false);
  let engine = $state<MaviSummaryEngine>("claude");
  let model = $state("haiku");

  $effect(() => {
    if (!open) return;
    const active = hasActivity;
    summarize = active ? untrack(() => maviPrefs.summarizeOnEnd) : false;
    engine = untrack(() => maviPrefs.summaryEngine);
    model = untrack(() => maviPrefs.summaryModel);
  });

  function cancel() {
    open = false;
    onclose?.();
  }

  function confirm() {
    const willSummarize = summarize && hasActivity;
    if (hasActivity) maviPrefs.summarizeOnEnd = summarize;
    maviPrefs.summaryEngine = engine;
    maviPrefs.summaryModel = model;
    open = false;
    onConfirm({
      summarize: willSummarize,
      engine,
      model: maviPrefs.summaryModel,
    });
  }
</script>

<Dialog bind:open size="md" title="End this thread's agent" {onclose}>
  {#if title}
    <p class="text-xs font-medium text-textPrimary truncate mb-2" {title}>{title}</p>
  {/if}

  {#if working}
    <div class="gp-notice text-meta mx-0" data-tone="warn" role="status">
      A turn is running. Ending the agent stops it and closes its process; the conversation is kept and can be resumed. To stop only this turn, use Stop beside the message box.
    </div>
  {:else}
    <p class="text-xs text-textMuted leading-relaxed">
      Closes this thread's agent process. The conversation is kept and can be resumed later.
    </p>
  {/if}

  <div class="rounded-xl bg-background/50 px-3 py-2.5 flex flex-col gap-2.5 mt-3">
    <Checkbox
      label="Summarize this session's work"
      description="Generates a concise Markdown summary using a small, fast model."
      checked={summarize}
      disabled={!hasActivity}
      onchange={(next) => (summarize = next)}
    />

    {#if summarize && hasActivity}
      <div class="flex flex-col gap-2 pt-1">
        <div class="flex items-center justify-between gap-2">
          <span class="gp-label">Engine</span>
          <div class="gp-segmented" data-size="sm" role="group" aria-label="Summary engine">
            <button
              type="button"
              class="gp-seg-btn justify-center"
              data-active={engine === "claude"}
              aria-pressed={engine === "claude"}
              onclick={() => (engine = "claude")}
            >
              Claude Haiku (fast)
            </button>
            <button
              type="button"
              class="gp-seg-btn justify-center"
              data-active={engine === "ai"}
              aria-pressed={engine === "ai"}
              onclick={() => (engine = "ai")}
            >
              Local / Manvi AI
            </button>
          </div>
        </div>

        {#if engine === "claude"}
          <div class="flex flex-wrap items-center justify-between gap-2">
            <span class="gp-label">Model</span>
            <div class="flex flex-wrap items-center gap-1.5" role="group" aria-label="Summary model">
              {#each CLAUDE_SUMMARY_MODELS as m (m)}
                <button
                  type="button"
                  class="gp-chip cursor-pointer font-mono"
                  data-kind="label"
                  data-tone={model === m ? "accent" : "neutral"}
                  aria-pressed={model === m}
                  onclick={() => (model = m)}
                >
                  {m}
                </button>
              {/each}
            </div>
          </div>
        {:else}
          <p class="text-micro text-textMuted leading-relaxed">
            Uses your configured Manvi or local loopback model server.
          </p>
        {/if}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    <button type="button" class="gp-btn" data-variant="ghost" onclick={cancel}>Cancel</button>
    <button
      type="button"
      class="gp-btn"
      data-variant="primary"
      data-testid="mavi-end-confirm"
      onclick={confirm}
    >
      {#if summarize && hasActivity}
        <Sparkles size={12} aria-hidden="true" />
        <span>End &amp; summarize</span>
      {:else}
        <Square size={12} aria-hidden="true" />
        <span>End agent</span>
      {/if}
    </button>
  {/snippet}
</Dialog>
