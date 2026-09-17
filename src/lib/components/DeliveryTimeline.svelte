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
   */
  import { CircleAlert, Radio, PauseCircle, GitCommitHorizontal } from "@lucide/svelte";
  import { keyedList } from "../ui/eachKeys";
  import { verdictRate } from "../delivery/transitions";
  import {
    barWidthPct,
    durationMs,
    formatDuration,
    longestDurationMs,
    medianSettledDurationMs,
    shortCommit,
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
  class="gp-card p-4 rounded-xl border border-border/80 bg-surface/50 shadow-xs flex flex-col gap-3"
  data-panel="delivery"
>
  <div class="flex items-center gap-2 border-b border-border/50 pb-2.5 flex-wrap">
    <GitCommitHorizontal size={15} class="text-accent shrink-0" />
    <span class="text-xs font-semibold text-textPrimary uppercase tracking-wider">{title}</span>
    {#if live.kind === "live"}
      <!-- The reason is the tooltip because it also says "retrying after N
           failure(s)": a poll that is struggling but has not given up is
           still live, and hiding that entirely would make the badge a
           promise it cannot keep. -->
      <span
        class="inline-flex items-center gap-1 text-[11px] text-amber-700 dark:text-amber-400"
        title={live.reason}
      >
        <Radio size={12} class="shrink-0" />
        Live
      </span>
    {:else if live.kind === "paused"}
      <!-- The case that must reach the screen: updates have stopped while
           rows keep rendering. Silence here is how stale reads as current. -->
      <span
        class="inline-flex items-center gap-1 text-[11px] text-textMuted"
        title={live.reason}
      >
        <PauseCircle size={12} class="shrink-0" />
        Live updates paused — {live.reason}
      </span>
    {/if}
  </div>

  {#if !checked}
    <!-- Could not check. Never an empty list: that would read as "nothing to
         report", which is a claim this report cannot make. -->
    <div class="flex items-start gap-2 text-xs py-3">
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
    <div class="flex items-center gap-4 flex-wrap text-[11px]">
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
         match the row order below it. -->
    <div class="flex items-center gap-0.5" aria-hidden="true">
      <!-- Unkeyed on purpose: these segments are decoration with no state and
           no focus to preserve, so there is no identity worth keying — and an
           unkeyed block cannot raise `each_key_duplicate` at all. The row list
           below is keyed, because its rows hold a focusable button. -->
      {#each rows as row}
        <span
          class="h-1.5 flex-1 rounded-sm {barClass(row.phase)}"
          style="min-width: 3px"
          title="{row.label} — {row.stateLabel}"
        ></span>
      {/each}
    </div>

    <div class="flex flex-col gap-1.5">
      {#each keyedList(rows, (row) => row.id) as { key, item: row } (key)}
        {@const ms = durationMs(row, now)}
        {@const width = barWidthPct(ms, longest)}
        <div class="flex items-center gap-2 text-xs min-w-0">
          <span class="w-44 shrink-0 min-w-0">
            {#if row.url}
              <button
                type="button"
                class="truncate max-w-full text-left text-textPrimary hover:text-accent hover:underline"
                title={row.label}
                onclick={() => void openExternal(row.url)}
              >
                {row.label}
              </button>
            {:else}
              <span class="block truncate text-textPrimary" title={row.label}>{row.label}</span>
            {/if}
            {#if row.sublabel}
              <span class="block truncate text-[10px] text-textMuted" title={row.sublabel}>
                {row.sublabel}
              </span>
            {/if}
          </span>

          <!-- `data-delivery-state` lets a rendered check read a row's verdict
               without matching against the card's own "Passed:" rate label,
               which contains the same word. -->
          <span
            class="w-28 shrink-0 truncate {textClass(row.phase)}"
            title={row.stateLabel}
            data-delivery-state={row.phase}
          >
            {row.stateLabel}
          </span>

          <!-- The bar is decoration; the duration beside it carries the value,
               and an unknown duration has no bar rather than a zero-width one
               that reads as instant. -->
          <span class="flex-1 h-1.5 rounded-full bg-border/40 overflow-hidden min-w-8">
            {#if width !== null}
              <span
                class="block h-full {barClass(row.phase)}"
                style="width: {width}%"
                data-delivery-bar={row.phase}
              ></span>
            {/if}
          </span>

          <span class="w-16 shrink-0 text-right font-mono text-textMuted">
            {formatDuration(ms)}
          </span>

          {#if row.commitSha}
            <span class="w-16 shrink-0 font-mono text-[10px] text-textMuted truncate" title={row.commitSha}>
              {shortCommit(row.commitSha)}
            </span>
          {/if}
          {#if row.branch}
            <span class="w-24 shrink-0 text-[10px] text-textMuted truncate" title={row.branch}>
              {row.branch}
            </span>
          {/if}
          {#if row.trigger}
            <span class="w-20 shrink-0 text-[10px] text-textMuted truncate" title={row.trigger}>
              {row.trigger}
            </span>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
