<script lang="ts">
  /**
   * The conversation. Follows new output while the reader is at the bottom,
   * and lets go the moment they scroll up or drag the scrollbar: streaming
   * text must never drag a reader away from what they are reading. A "Latest"
   * pill brings them back. A smooth jump to the latest line is not mistaken
   * for the reader scrolling up while it is under way.
   *
   * Assistant prose streams as plain text and is rendered as Markdown once
   * its block is complete: rendering crosses IPC, and doing it per delta
   * would re-parse the whole message on every token.
   */
  import { onDestroy, tick, untrack } from "svelte";
  import {
    ArrowDown,
    ArrowRight,
    Brain,
    ChevronRight,
    CircleCheck,
    CircleStop,
    CircleX,
    Info,
    LoaderCircle,
    TriangleAlert,
  } from "@lucide/svelte";
  import MarkdownBody from "../MarkdownBody.svelte";
  import MaviToolGroup from "./MaviToolGroup.svelte";
  import MaviApproval from "./MaviApproval.svelte";
  import MaviSessionSummary from "./MaviSessionSummary.svelte";
  import MaviQuestion from "./MaviQuestion.svelte";
  import { ASK_TOOL, encodeAnswers, parseQuestions } from "../../mavi/questions";
  import { toRows } from "../../mavi/rows";
  import { allTurnTotals } from "../../mavi/changes";
  import {
    JUMP_TIMEOUT_MS,
    createScrollPinState,
    markProgrammaticScroll,
    onJumpTimeout,
    onPointerEnd as reducePointerEnd,
    onScroll as reduceScroll,
    onScrollEnd as reduceScrollEnd,
    onUserIntent as reduceUserIntent,
    setAutoScroll,
    shouldAutoScrollOnResize,
    shouldShowLatestPill,
    startJump,
    syncMetrics,
    type ScrollMetrics,
  } from "../../mavi/scrollPin";
  import { LAYERS } from "../../ui/layers";
  import type { ThreadState, ThreadWorkSummary, TurnEndItem } from "../../mavi/types";

  let {
    thread,
    root,
    working,
    autoScroll = true,
    showThinking = true,
    expandTools = false,
    showTurnMeta = true,
    textSize = "md",
    messageStyle = "bubbles",
    workSummary = null,
    summarizing = false,
    summaryError = null,
    onAnswer,
    onShowTurn,
    onOpenFile,
    onRegenerateSummary,
    onDismissSummary,
  }: {
    thread: ThreadState;
    root: string | null;
    working: boolean;
    autoScroll?: boolean;
    showThinking?: boolean;
    expandTools?: boolean;
    showTurnMeta?: boolean;
    textSize?: "sm" | "md" | "lg";
    messageStyle?: "bubbles" | "editorial";
    workSummary?: ThreadWorkSummary | null;
    summarizing?: boolean;
    summaryError?: string | null;
    onAnswer: (requestId: string, decision: "allow" | "deny", message?: string) => Promise<void>;
    onShowTurn: (turn: number) => void;
    onOpenFile: (path: string) => void;
    onRegenerateSummary?: () => void;
    onDismissSummary?: () => void;
  } = $props();

  let scroller: HTMLDivElement | undefined = $state();
  let lane: HTMLDivElement | undefined = $state();
  let pin = $state(createScrollPinState({ autoScroll: untrack(() => autoScroll) }));
  let openThinking = $state<Record<string, boolean>>({});

  let rows = $derived(toRows(thread.items, root));
  /** Every turn's footer totals, in one pass. */
  let totals = $derived(allTurnTotals(thread, root));
  let showLatestPill = $derived(shouldShowLatestPill(pin));
  let textClass = $derived(textSize === "sm" ? "text-xs" : textSize === "lg" ? "text-base" : "text-sm");

  /** Changes whenever the visible tail grows: a new row, more streamed text, or session summary state. */
  let tailKey = $derived.by(() => {
    const last = thread.items[thread.items.length - 1];
    const size = last && (last.kind === "assistant" || last.kind === "thinking") ? last.text.length : 0;
    return `${thread.items.length}:${size}:${working}:${summarizing}:${workSummary?.createdAt ?? 0}:${workSummary?.text.length ?? 0}:${summaryError ?? ""}`;
  });

  let prevTailKey = "";
  let lastTailGrowthAt = 0;
  let initialSettlingUntil = untrack(() => autoScroll) ? performance.now() + 1200 : 0;
  let prevAutoScroll = untrack(() => autoScroll);
  let jumpTimer: ReturnType<typeof setTimeout> | undefined;

  function clearJumpTimer() {
    if (jumpTimer !== undefined) {
      clearTimeout(jumpTimer);
      jumpTimer = undefined;
    }
  }

  function scheduleJumpTimer(delayMs: number) {
    clearJumpTimer();
    jumpTimer = setTimeout(() => {
      jumpTimer = undefined;
      pin = onJumpTimeout(pin, scroller ? readMetrics(scroller) : null, performance.now(), autoScroll);
    }, Math.max(16, delayMs));
  }

  onDestroy(clearJumpTimer);

  function readMetrics(el: HTMLElement): ScrollMetrics {
    return {
      scrollTop: el.scrollTop,
      scrollHeight: el.scrollHeight,
      clientHeight: el.clientHeight,
    };
  }

  function onScroll() {
    if (!scroller) return;
    const now = performance.now();
    pin = reduceScroll(pin, readMetrics(scroller), now, autoScroll);
    if (!pin.pinned) initialSettlingUntil = 0;
    if (pin.jumping) {
      scheduleJumpTimer(pin.jumpExpiresAt - now);
    } else {
      clearJumpTimer();
    }
  }

  function onScrollEnd() {
    if (!scroller) return;
    pin = reduceScrollEnd(pin, readMetrics(scroller), autoScroll);
    if (!pin.pinned) initialSettlingUntil = 0;
    clearJumpTimer();
  }

  function onUserIntent(event: WheelEvent | KeyboardEvent | PointerEvent | TouchEvent) {
    const now = performance.now();
    initialSettlingUntil = 0;
    if (event instanceof WheelEvent) {
      pin = reduceUserIntent(pin, { kind: "wheel", deltaY: event.deltaY }, now);
    } else if (event instanceof KeyboardEvent) {
      pin = reduceUserIntent(pin, { kind: "key", key: event.key }, now);
    } else if (typeof TouchEvent !== "undefined" && event instanceof TouchEvent) {
      pin = reduceUserIntent(pin, { kind: "touch" }, now);
    } else {
      pin = reduceUserIntent(pin, { kind: "pointer" }, now);
    }
    if (!pin.jumping) clearJumpTimer();
  }

  function onPointerRelease() {
    if (!pin.pointerActive) return;
    pin = reducePointerEnd(pin, scroller ? readMetrics(scroller) : null, performance.now(), autoScroll);
  }

  async function toBottom() {
    await tick();
    if (!scroller) return;
    // Re-check after tick: user may have scrolled up, grabbed the scrollbar, or disabled autoScroll.
    if (!pin.pinned || !autoScroll || pin.pointerActive || pin.jumping) {
      pin = syncMetrics(pin, readMetrics(scroller), autoScroll);
      return;
    }
    const metrics = readMetrics(scroller);
    const targetTop = Math.max(0, metrics.scrollHeight - metrics.clientHeight);
    if (Math.abs(scroller.scrollTop - targetTop) <= 1) {
      pin = syncMetrics(pin, metrics, autoScroll);
      return;
    }
    pin = markProgrammaticScroll(pin, targetTop, performance.now(), metrics);
    scroller.scrollTop = targetTop;
  }

  $effect(() => {
    const nextAutoScroll = autoScroll;
    if (nextAutoScroll === prevAutoScroll) return;
    prevAutoScroll = nextAutoScroll;
    untrack(() => {
      pin = setAutoScroll(pin, nextAutoScroll, scroller ? readMetrics(scroller) : undefined);
      if (nextAutoScroll && pin.pinned && !pin.pointerActive) void toBottom();
    });
  });

  $effect(() => {
    const nextKey = tailKey;
    const shouldFollow = autoScroll;
    if (nextKey === prevTailKey) return;
    prevTailKey = nextKey;
    lastTailGrowthAt = performance.now();
    untrack(() => {
      if (shouldFollow && pin.pinned && !pin.pointerActive && !pin.jumping) {
        void toBottom();
      } else {
        void tick().then(() => {
          if (scroller) pin = syncMetrics(pin, readMetrics(scroller), shouldFollow);
        });
      }
    });
  });

  // Content also grows after it is added (Markdown renders asynchronously):
  // follow only while pinned, not dragging, autoScroll is enabled, and the
  // agent is working or the tail just grew (never when expanding an old block).
  function handleLaneResize() {
    if (!scroller) return;
    const now = performance.now();
    if (!pin.pinned) initialSettlingUntil = 0;
    const tailRecentlyGrew = now - lastTailGrowthAt < 300 || now < initialSettlingUntil;
    if (shouldAutoScrollOnResize(pin, autoScroll, working, tailRecentlyGrew)) {
      void toBottom();
    } else {
      pin = syncMetrics(pin, readMetrics(scroller), autoScroll);
    }
  }

  $effect(() => {
    if (!lane || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(handleLaneResize);
    observer.observe(lane);
    return () => observer.disconnect();
  });

  /**
   * The one thing a screen reader hears unprompted: the thread's state when it
   * changes (working, waiting for an answer, done), never each tool or token,
   * which the visible activity lines show.
   */
  let announcement = $derived.by(() => {
    if (thread.items.some((i) => i.kind === "tool" && i.approval?.state === "open")) return "The agent is waiting for your answer.";
    if (working) return "The agent is working.";
    if (thread.ended) return "The agent ended.";
    return thread.turn > 0 ? "The agent finished the turn." : "";
  });

  function jump() {
    if (!scroller) return;
    const now = performance.now();
    pin = startJump(pin, now);
    scheduleJumpTimer(JUMP_TIMEOUT_MS);
    const targetTop = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
    scroller.scrollTo({ top: targetTop, behavior: "smooth" });
  }

  function firstThoughtLine(text: string): string {
    return text.split("\n").map((line) => line.trim()).find(Boolean) ?? "";
  }

  function formatDuration(ms: number | null): string {
    if (ms === null) return "";
    if (ms < 1000) return `${ms} ms`;
    const s = Math.round(ms / 1000);
    return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
  }

  function endLabel(end: TurnEndItem): string {
    if (end.interrupted) return "Stopped";
    // From the transcript: it finished, but when and at what cost is not recorded per turn.
    if (end.history) return "Earlier turn";
    if (!end.ok) return end.subtype === "error_max_turns" ? "Hit the turn limit" : "Ended with an error";
    return end.summary?.category === "review_ready" ? "Ready for review" : end.summary?.category === "blocked" ? "Blocked" : "Done";
  }
</script>

<svelte:window onpointerup={onPointerRelease} onpointercancel={onPointerRelease} ontouchend={onPointerRelease} />

<div class="mavi-timeline-wrap relative flex-1 min-h-0">
  <!-- Justified: the wheel, touch, pointer and key listeners only observe the
       reader's intent to scroll up or cancel an active jump; they handle nothing themselves. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    bind:this={scroller}
    class="mavi-timeline h-full overflow-y-auto"
    style:overflow-anchor={pin.pinned ? "none" : "auto"}
    onscroll={onScroll}
    onscrollend={onScrollEnd}
    onwheel={onUserIntent}
    onkeydown={onUserIntent}
    onpointerdown={onUserIntent}
    ontouchstart={onUserIntent}
    role="log"
    aria-live="off"
    aria-label="Conversation"
  >
    <div class="mavi-lane" bind:this={lane}>
      {#if rows.length === 0}
        <p class="text-xs text-textMuted py-8 text-center">The conversation starts with your first message.</p>
      {/if}
      {#each rows as row (row.key)}
        {#if row.kind === "tools"}
          <div class="mavi-row">
            <MaviToolGroup
              tools={row.tools}
              summary={row.summary}
              failed={row.failed}
              running={row.running}
              editedFiles={row.editedFiles}
              {root}
              activity={row.running ? thread.activity : null}
              defaultExpanded={expandTools}
              {onOpenFile}
            />
          </div>
        {:else if row.kind === "approval"}
          <div class="mavi-row">
            {#if row.tool.name === ASK_TOOL && parseQuestions(row.tool.input)}
              <MaviQuestion
                tool={row.tool}
                onSubmit={(answers) => onAnswer(row.tool.approval!.requestId, "allow", encodeAnswers(answers))}
                onDecline={() => onAnswer(row.tool.approval!.requestId, "deny")}
              />
            {:else}
              <MaviApproval
                tool={row.tool}
                {root}
                onAnswer={(decision, message) => onAnswer(row.tool.approval!.requestId, decision, message)}
              />
            {/if}
          </div>
        {:else if row.kind === "question"}
          <div class="mavi-row"><MaviQuestion tool={row.tool} /></div>
        {:else if row.item.kind === "user"}
          {#if messageStyle === "editorial"}
            <div class="mavi-row mavi-user-editorial {textClass} text-textPrimary whitespace-pre-wrap select-text">{row.item.text}</div>
          {:else}
            <div class="mavi-row mavi-user-row">
              <div class="mavi-user {textClass} text-textPrimary whitespace-pre-wrap select-text">{row.item.text}</div>
            </div>
          {/if}
        {:else if row.item.kind === "assistant"}
          <div class="mavi-row {textClass} text-textPrimary">
            {#if row.item.final && !row.item.streaming}
              <MarkdownBody source={row.item.text} class="{textClass} text-textPrimary select-text" />
            {:else}
              <div class="whitespace-pre-wrap select-text">{row.item.text}<span class="mavi-caret animate-pulse" aria-hidden="true"></span></div>
            {/if}
          </div>
        {:else if row.item.kind === "thinking" && showThinking}
          {#if row.item.streaming}
            <div class="mavi-row flex items-center gap-1.5 text-meta text-textMuted">
              <Brain size={12} class="shrink-0" aria-hidden="true" />
              <span class="animate-pulse">Thinking…</span>
            </div>
          {:else if row.item.text.trim()}
            {@const thoughtOpen = !!openThinking[row.item.id]}
            <div class="mavi-row">
              <button
                type="button"
                class="mavi-thinking-head flex items-center gap-1.5 w-full text-left text-meta text-textMuted"
                aria-expanded={thoughtOpen}
                onclick={() => (openThinking[row.item.id] = !thoughtOpen)}
              >
                <ChevronRight size={12} class="mavi-chevron shrink-0 {thoughtOpen ? 'mavi-chevron-open' : ''}" aria-hidden="true" />
                <Brain size={12} class="shrink-0" aria-hidden="true" />
                <span class="truncate">{firstThoughtLine(row.item.text)}</span>
              </button>
              {#if thoughtOpen}
                <div class="mavi-thinking-body text-meta text-textMuted whitespace-pre-wrap select-text mt-1">
                  {row.item.text}
                </div>
              {/if}
            </div>
          {/if}
        {:else if row.item.kind === "notice"}
          <div
            class="mavi-row gp-notice whitespace-pre-wrap"
            data-tone={row.item.tone === "error" ? "danger" : row.item.tone === "warn" ? "warn" : "info"}
            role={row.item.tone === "error" ? "alert" : "status"}
          >
            <span class="inline-flex items-start gap-1.5">
              {#if row.item.tone === "info"}<Info size={12} class="mt-0.5 shrink-0" aria-hidden="true" />{:else}<TriangleAlert size={12} class="mt-0.5 shrink-0" aria-hidden="true" />{/if}
              <span>{row.item.text}</span>
            </span>
          </div>
        {:else if row.item.kind === "turn_end"}
          {@const end = row.item}
          {@const turnTotal = totals.get(end.turn)}
          {@const files = thread.turnFiles[end.turn]}
          <div class="mavi-row mavi-turn-end flex flex-wrap items-center gap-2 text-meta text-textMuted">
            {#if end.interrupted}
              <CircleStop size={12} class="text-warn shrink-0" aria-hidden="true" />
            {:else if end.ok}
              <CircleCheck size={12} class="text-ok shrink-0" aria-hidden="true" />
            {:else}
              <CircleX size={12} class="text-danger shrink-0" aria-hidden="true" />
            {/if}
            <span class="font-medium text-textPrimary">{endLabel(end)}</span>
            {#if showTurnMeta && end.durationMs !== null}<span>· {formatDuration(end.durationMs)}</span>{/if}
            {#if showTurnMeta && end.costUsd !== null && end.costUsd > 0}<span>· ${end.costUsd.toFixed(2)}</span>{/if}
            {#if turnTotal && turnTotal.files > 0}
              <button class="gp-chip" data-tone="accent" onclick={() => onShowTurn(end.turn)} title="Review what this turn changed">
                {turnTotal.files} {turnTotal.files === 1 ? "file" : "files"}
                {#if turnTotal.added > 0}<span class="text-ok">+{turnTotal.added}</span>{/if}
                {#if turnTotal.removed > 0}<span class="text-danger">−{turnTotal.removed}</span>{/if}
              </button>
            {:else if files && !files.available}
              <span>· changes outside edit tools not tracked</span>
            {/if}
            {#if end.summary?.needsAction}
              <div class="mavi-next w-full flex items-start gap-1.5 text-meta">
                <ArrowRight size={12} class="text-accent shrink-0 mt-0.5" aria-hidden="true" />
                <span class="text-textPrimary"><span class="font-medium text-accent">Next:</span> {end.summary.needsAction}</span>
              </div>
            {/if}
          </div>
        {/if}
      {/each}
      {#if working && thread.items[thread.items.length - 1]?.kind !== "tool"}
        <div class="mavi-row">
          <span class="gp-chip" data-tone="accent" role="status">
            <LoaderCircle size={11} class="shrink-0 animate-spin" aria-hidden="true" />
            <span class="truncate">{thread.activity ?? "Working…"}</span>
          </span>
        </div>
      {/if}
      {#if summarizing || summaryError || workSummary}
        <div class="mavi-row">
          <MaviSessionSummary
            summary={workSummary}
            loading={summarizing}
            error={summaryError}
            onRegenerate={() => onRegenerateSummary?.()}
            onDismiss={() => onDismissSummary?.()}
          />
        </div>
      {/if}
    </div>
  </div>
  <div class="sr-only" aria-live="polite">{announcement}</div>
  {#if showLatestPill}
    <button
      class="mavi-latest gp-btn shadow-float"
      data-variant="secondary"
      style:z-index={LAYERS.STICKY}
      onclick={jump}
    >
      <ArrowDown size={12} aria-hidden="true" /> Latest
    </button>
  {/if}
</div>

<style>
  .mavi-timeline {
    overscroll-behavior-y: contain;
    scrollbar-gutter: stable;
  }
  .mavi-lane {
    max-width: var(--mavi-lane-max, 52rem);
    margin: 0 auto;
    padding: 16px 20px 40px;
  }
  .mavi-row {
    margin-top: var(--mavi-row-gap, 10px);
  }
  .mavi-user-row {
    display: flex;
    justify-content: flex-end;
    margin-top: var(--mavi-turn-gap, 24px);
  }
  .mavi-user {
    max-width: 85%;
    padding: var(--mavi-bubble-pad, 10px 14px);
    border-radius: var(--gp-r-lg);
    background: rgb(var(--c-surface) / 0.9);
    border: 1px solid var(--gp-edge);
  }
  .mavi-user-editorial {
    margin-top: var(--mavi-turn-gap, 24px);
    padding: var(--mavi-bubble-pad, 10px 14px);
    border-left: 2px solid rgb(var(--c-accent) / 0.65);
    border-radius: 0 var(--gp-r-md) var(--gp-r-md) 0;
    background: rgb(var(--c-surface) / 0.55);
  }
  .mavi-thinking-head {
    padding: 2px 6px;
    margin-left: -6px;
    border-radius: var(--gp-r-sm);
    transition: color var(--gp-dur-fast) var(--gp-ease-effect), background-color var(--gp-dur-fast) var(--gp-ease-effect);
  }
  .mavi-thinking-head:hover {
    color: rgb(var(--c-text));
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  .mavi-thinking-head :global(.mavi-chevron) {
    transition: transform var(--gp-dur-fast) var(--gp-ease-effect);
  }
  .mavi-thinking-head :global(.mavi-chevron-open) {
    transform: rotate(90deg);
  }
  .mavi-thinking-body {
    padding: 8px 12px;
    border-radius: var(--gp-r-md);
    background: rgb(var(--c-surface) / 0.65);
    border: 1px solid var(--gp-edge-soft);
    max-height: 18rem;
    overflow-y: auto;
  }
  .mavi-turn-end {
    padding-bottom: 8px;
    border-bottom: 1px dashed var(--gp-edge-soft);
  }
  .mavi-next {
    padding: 6px 10px;
    border-radius: var(--gp-r-md);
    background: rgb(var(--c-accent) / 0.08);
    border: 1px solid rgb(var(--c-accent) / 0.35);
  }
  .mavi-caret {
    display: inline-block;
    width: 0.45em;
    height: 1em;
    margin-left: 1px;
    vertical-align: text-bottom;
    background: rgb(var(--c-accent) / 0.65);
  }
  .mavi-latest {
    position: absolute;
    bottom: 12px;
    left: 50%;
    transform: translateX(-50%);
  }
</style>
