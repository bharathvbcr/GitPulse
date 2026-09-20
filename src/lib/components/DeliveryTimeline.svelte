<script lang="ts">
  /**
   * One visualization for both delivery sources.
   *
   * A workflow run and an App Hosting rollout answer the same question — did
   * this attempt start, how long did it take, how did it end — so they are
   * drawn by one component rather than two that drift apart. The source-specific
   * vocabularies are resolved to a `TimelineRow` before they arrive here.
   *
   * Every empty state in here is a different fact, and the whole point is that
   * they do not look alike:
   *
   *  - `checked: false` means the listing could not run. It renders as a
   *    failure with its reason, never as an empty list — replacing real rows
   *    with a confident "nothing here" is the exact dishonesty this component
   *    was written to avoid.
   *  - `rows` empty with `checked: true` is a real, measured absence.
   *  - `truncated` means the sample is the newest slice of a longer history,
   *    so every figure derived from it is labelled with its bound.
   *  - a null pass rate means nothing in the sample could be judged, which is
   *    not the same as a measured 0%.
   *
   * The duration list previews then expands. The sample figures, the outcome
   * strip, and the bar scale stay on the full `rows` — a collapsed preview
   * that also shrank the rate would lie about the repository.
   */
  import { CircleAlert, Radio, PauseCircle, GitCommitHorizontal } from "@lucide/svelte";
  import { keyedList } from "../ui/eachKeys";
  import { expandLabel, overflowsPreview, previewSlice } from "../ui/previewList";
  import { verdictRate } from "../delivery/transitions";
  import {
    barWidthPct,
    durationMs,
    formatDuration,
    glanceName,
    glanceState,
    glanceTitle,
    longestDurationMs,
    medianSettledDurationMs,
    shortCommit,
    TIMELINE_PREVIEW_COUNT,
    type TimelineRow,
  } from "../delivery/timeline";
  import { openExternal } from "../desktop/openExternal";

  /** How the live poll is doing, so a stopped one can say so. */
  export interface LiveState {
    kind: "live" | "idle" | "paused";
    reason: string;
  }

  let {
    title,
    rows = [],
    now,
    checked = true,
    truncated = false,
    error = null,
    sampleNoun = "runs",
    live = { kind: "idle", reason: "" },
  }: {
    title: string;
    rows: readonly TimelineRow[];
    now: number;
    checked?: boolean;
    truncated?: boolean;
    error?: string | null;
    sampleNoun?: string;
    live?: LiveState;
  } = $props();

  let expanded = $state(false);
  const shown = $derived(previewSlice(rows, expanded, TIMELINE_PREVIEW_COUNT));
  const glanceCols = $derived(Math.min(3, Math.max(1, shown.length)));
  const rate = $derived(verdictRate(rows));
  const median = $derived(medianSettledDurationMs(rows, now));
  const longest = $derived(longestDurationMs(rows, now));

  /**
   * Bar colour per phase. Both shades on every verdict, matching
   * `runStateClass` and `rolloutStateClass` — a bare `-400` is tuned for the
   * dark theme and washes out on the light theme's near-white card.
   */
  function barClass(phase: TimelineRow["phase"]): string {
    if (phase === "settled_ok") return "bg-green-600 dark:bg-green-500";
    if (phase === "settled_bad") return "bg-red-600 dark:bg-red-500";
    if (phase === "in_flight") return "bg-amber-500 dark:bg-amber-400";
    // No verdict to colour: a neutral track, visibly not a pass or a fail.
    return "bg-border";
  }

  function textClass(phase: TimelineRow["phase"]): string {
    if (phase === "settled_ok") return "text-green-700 dark:text-green-400";
    if (phase === "settled_bad") return "text-red-700 dark:text-red-400";
    if (phase === "in_flight") return "text-amber-700 dark:text-amber-400";
    return "text-textMuted";
  }
</script>

<div
  class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-xs flex flex-col gap-3 min-w-0 w-full"
  data-panel="delivery"
>
  <div class="flex items-center gap-2 border-b border-border/50 pb-2.5 flex-wrap min-w-0">
    <GitCommitHorizontal size={15} class="text-accent shrink-0" />
    <span class="text-xs font-semibold text-textPrimary uppercase tracking-wider min-w-0 truncate">{title}</span>
    {#if live.kind === "live"}
      <!-- The reason is the tooltip because it also says "retrying after N
           failure(s)": a poll that is struggling but has not given up is
           still live, and hiding that entirely would make the badge a
           promise it cannot keep. -->
      <span
        class="inline-flex items-center gap-1 text-[11px] text-amber-700 dark:text-amber-400 min-w-0"
        title={live.reason}
      >
        <Radio size={12} class="shrink-0" />
        Live
      </span>
    {:else if live.kind === "paused"}
      <!-- The case that must reach the screen: updates have stopped while
           rows keep rendering. Silence here is how stale reads as current. -->
      <span
        class="inline-flex items-center gap-1 text-[11px] text-textMuted min-w-0"
        title={live.reason}
      >
        <PauseCircle size={12} class="shrink-0" />
        <span class="min-w-0 wrap-break-word">Live updates paused — {live.reason}</span>
      </span>
    {/if}
  </div>

  {#if !checked}
    <!-- Could not check. Never an empty list: that would read as "nothing to
         report", which is a claim this report cannot make. -->
    <div class="flex items-start gap-2 text-xs py-3 min-w-0">
      <CircleAlert size={14} class="text-rose-500 shrink-0 mt-0.5" />
      <div class="min-w-0">
        <p class="text-rose-600 dark:text-rose-400 font-medium">
          Could not read {sampleNoun} — this is not an absence of {sampleNoun}.
        </p>
        {#if error}
          <p class="font-mono text-[11px] text-textMuted wrap-break-word mt-1">{error}</p>
        {/if}
      </div>
    </div>
  {:else if rows.length === 0}
    <p class="text-xs text-textMuted py-3">No {sampleNoun} recorded for this repository.</p>
  {:else}
    <!-- Derived figures, each stated with the sample behind it. A rate with no
         denominator is the shape that lets 1-of-1 read like 200-of-200. -->
    <div class="flex items-center gap-x-4 gap-y-1 flex-wrap text-[11px] min-w-0">
      <span class="text-textMuted">
        Passed:
        {#if rate.ratePct === null}
          <span class="text-textMuted font-medium">no judged {sampleNoun}</span>
        {:else}
          <span class="text-textPrimary font-mono font-medium">{rate.ratePct}%</span>
          <span class="text-textMuted">({rate.passed}/{rate.judged})</span>
        {/if}
      </span>
      <span class="text-textMuted">
        Median:
        <span class="text-textPrimary font-mono font-medium">{formatDuration(median.medianMs)}</span>
        <span class="text-textMuted">(n={median.sample})</span>
      </span>
      {#if rate.unjudged > 0}
        <span class="text-textMuted" title="In flight, or a state this build cannot judge">
          Unjudged: <span class="font-mono">{rate.unjudged}</span>
        </span>
      {/if}
      <span class="text-textMuted">
        Sample: last <span class="font-mono">{rows.length}</span>
        {#if truncated}
          <span class="text-amber-700 dark:text-amber-400">(capped — more exist)</span>
        {/if}
      </span>
    </div>

    <!-- Outcome strip: the last N verdicts at a glance, newest on the left to
         match the row order below it. Stays on the full sample so collapsing
         the list cannot hide a red run that is still in the rate. -->
    <div class="flex items-center gap-0.5 min-w-0 overflow-hidden" aria-hidden="true" data-delivery-strip>
      <!-- Unkeyed on purpose: these segments are decoration with no state and
           no focus to preserve, so there is no identity worth keying — and an
           unkeyed block cannot raise `each_key_duplicate` at all. The row list
           below is keyed, because its rows hold a focusable button. -->
      {#each rows as row}
        <span
          class="h-1.5 flex-1 rounded-sm min-w-[3px] {barClass(row.phase)}"
          data-delivery-strip-seg={row.phase}
          title="{row.label} — {row.stateLabel}"
        ></span>
      {/each}
    </div>

    <!-- Glance tiles: verdict, duration, identity, SHA. The commit subject
         and the source's long state sentence stay on `title` so a collapsed
         preview can be read in one look. Three columns match the preview
         count; fewer runs shrink the grid rather than leaving empty cells. -->
    <div
      class="grid gap-1.5 min-w-0"
      style="grid-template-columns: repeat({glanceCols}, minmax(0, 1fr))"
      data-delivery-glances
    >
      {#each keyedList(shown, (row) => row.id) as { key, item: row } (key)}
        {@const ms = durationMs(row, now)}
        {@const width = barWidthPct(ms, longest)}
        {@const name = glanceName(row)}
        {@const tip = glanceTitle(row)}
        {@const tileClass = "flex flex-col gap-1 min-w-0 p-1.5 rounded-lg border border-border/60 bg-surface/40 text-left"}
        {#snippet glanceBody()}
          <div class="flex items-center gap-1 min-w-0">
            <span class="size-1.5 rounded-full shrink-0 {barClass(row.phase)}" aria-hidden="true"></span>
            <!-- `data-delivery-state` lets a rendered check read a row's
                 verdict without matching the card's "Passed:" rate label. -->
            <span class="text-[10px] font-medium truncate {textClass(row.phase)}" data-delivery-state={row.phase}>
              {glanceState(row.phase)}
            </span>
            <span class="ml-auto font-mono tabular-nums text-[11px] text-textPrimary shrink-0 whitespace-nowrap">
              {formatDuration(ms)}
            </span>
          </div>
          <span class="h-1 rounded-full bg-border/40 overflow-hidden min-w-0">
            {#if width !== null}
              <span
                class="block h-full {barClass(row.phase)}"
                style="width: {width}%"
                data-delivery-bar={row.phase}
              ></span>
            {/if}
          </span>
          <div class="flex items-center gap-1 min-w-0 text-[10px] text-textMuted">
            {#if name}
              <span class="truncate min-w-0">{name}</span>
            {/if}
            {#if row.commitSha}
              <span class="font-mono shrink-0">{shortCommit(row.commitSha)}</span>
            {/if}
          </div>
        {/snippet}
        {#if row.url}
          <button
            type="button"
            class="{tileClass} hover:border-accent/50 hover:bg-surface/80 transition-colors"
            data-delivery-row={row.id}
            title={tip}
            aria-label="{name} {glanceState(row.phase)} {formatDuration(ms)}"
            onclick={() => void openExternal(row.url)}
          >
            {@render glanceBody()}
          </button>
        {:else}
          <div class={tileClass} data-delivery-row={row.id} title={tip}>
            {@render glanceBody()}
          </div>
        {/if}
      {/each}
    </div>

    {#if overflowsPreview(rows.length, TIMELINE_PREVIEW_COUNT)}
      <button
        type="button"
        class="w-full px-2 py-1 rounded-xl border border-dashed border-border/80 text-[11px] text-textMuted hover:text-textPrimary hover:border-accent/50 transition-colors"
        data-delivery-expand
        aria-expanded={expanded}
        onclick={() => (expanded = !expanded)}
      >
        {expandLabel(rows.length, expanded, sampleNoun)}
      </button>
    {/if}
  {/if}
</div>
