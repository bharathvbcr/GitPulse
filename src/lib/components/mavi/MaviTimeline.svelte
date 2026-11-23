<script lang="ts">
  /**
   * The conversation. Follows new output while the reader is at the bottom,
   * and lets go the moment they scroll up: streaming text must never drag a
   * reader away from what they are reading. A "Latest" pill brings them
   * back. Scrolls this component makes are fenced so they are not mistaken
   * for the reader's.
   *
   * Assistant prose streams as plain text and is rendered as Markdown once
   * its block is complete: rendering crosses IPC, and doing it per delta
   * would re-parse the whole message on every token.
   */
  import { tick } from "svelte";
  import { ArrowDown, CircleCheck, CircleX, CircleStop, Info, TriangleAlert, Brain } from "@lucide/svelte";
  import MarkdownBody from "../MarkdownBody.svelte";
  import MaviToolGroup from "./MaviToolGroup.svelte";
  import MaviApproval from "./MaviApproval.svelte";
  import { toRows } from "../../mavi/rows";
  import { turnTotals } from "../../mavi/changes";
  import type { ThreadState, TurnEndItem } from "../../mavi/types";

  let {
    thread,
    root,
    working,
    onAnswer,
    onShowTurn,
    onOpenFile,
  }: {
    thread: ThreadState;
    root: string | null;
    working: boolean;
    onAnswer: (requestId: string, decision: "allow" | "deny", message?: string) => Promise<void>;
    onShowTurn: (turn: number) => void;
    onOpenFile: (path: string) => void;
  } = $props();

  let scroller: HTMLDivElement | undefined = $state();
  let pinned = $state(true);
  let fence = 0;

  let rows = $derived(toRows(thread.items, root));
  /** Changes whenever the visible tail grows: a new row or more streamed text. */
  let tailKey = $derived.by(() => {
    const last = thread.items[thread.items.length - 1];
    const size = last && (last.kind === "assistant" || last.kind === "thinking") ? last.text.length : 0;
    return `${thread.items.length}:${size}:${working}`;
  });

  function atBottom(el: HTMLElement): boolean {
    return el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  }

  function onScroll() {
    if (!scroller) return;
    if (fence > 0) {
      fence -= 1;
      return;
    }
    pinned = atBottom(scroller);
  }

  function onUserIntent(event: WheelEvent | KeyboardEvent | PointerEvent) {
    // Any upward intent releases at once, before the scroll event lands.
    if (event instanceof WheelEvent && event.deltaY < 0) pinned = false;
    if (event instanceof KeyboardEvent && ["PageUp", "ArrowUp", "Home"].includes(event.key)) pinned = false;
  }

  async function toBottom(smooth = false) {
    await tick();
    if (!scroller) return;
    fence += 1;
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: smooth ? "smooth" : "auto" });
  }

  $effect(() => {
    void tailKey;
    if (pinned) void toBottom();
  });

  // Content also grows after it is added (Markdown renders asynchronously, a
  // group expands): a reader at the bottom stays there.
  let lane: HTMLDivElement | undefined = $state();
  $effect(() => {
    if (!lane || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      if (pinned) void toBottom();
    });
    observer.observe(lane);
    return () => observer.disconnect();
  });

  function jump() {
    pinned = true;
    void toBottom(true);
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

<div class="mavi-timeline-wrap relative flex-1 min-h-0">
  <!-- Justified: the wheel and key listeners only observe the reader's intent
       to scroll up (to stop following); they handle nothing themselves. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    bind:this={scroller}
    class="mavi-timeline h-full overflow-y-auto"
    onscroll={onScroll}
    onwheel={onUserIntent}
    onkeydown={onUserIntent}
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
              {onOpenFile}
            />
          </div>
        {:else if row.kind === "approval"}
          <div class="mavi-row">
            <MaviApproval
              tool={row.tool}
              {root}
              onAnswer={(decision, message) => onAnswer(row.tool.approval!.requestId, decision, message)}
            />
          </div>
        {:else if row.item.kind === "user"}
          <div class="mavi-row mavi-user-row">
            <div class="mavi-user text-sm text-textPrimary whitespace-pre-wrap select-text">{row.item.text}</div>
          </div>
        {:else if row.item.kind === "assistant"}
          <div class="mavi-row text-sm text-textPrimary">
            {#if row.item.final && !row.item.streaming}
              <MarkdownBody source={row.item.text} class="text-sm text-textPrimary select-text" />
            {:else}
              <div class="whitespace-pre-wrap select-text">{row.item.text}<span class="mavi-caret animate-pulse" aria-hidden="true"></span></div>
            {/if}
          </div>
        {:else if row.item.kind === "thinking"}
          <div class="mavi-row flex items-center gap-1.5 text-meta text-textMuted">
            <Brain size={12} aria-hidden="true" />
            {#if row.item.streaming}
              <span class="animate-pulse">Thinking</span>
            {:else}
              <span class="truncate" title={row.item.text}>{row.item.text.split("\n")[0]}</span>
            {/if}
          </div>
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
          {@const totals = turnTotals(thread, root, end.turn)}
          {@const files = thread.turnFiles[end.turn]}
          <div class="mavi-row mavi-turn-end flex flex-wrap items-center gap-2 text-meta text-textMuted">
            {#if end.interrupted}
              <CircleStop size={12} class="text-warn" aria-hidden="true" />
            {:else if end.ok}
              <CircleCheck size={12} class="text-ok" aria-hidden="true" />
            {:else}
              <CircleX size={12} class="text-danger" aria-hidden="true" />
            {/if}
            <span>{endLabel(end)}</span>
            {#if end.durationMs !== null}<span>· {formatDuration(end.durationMs)}</span>{/if}
            {#if end.costUsd !== null && end.costUsd > 0}<span>· ${end.costUsd.toFixed(2)}</span>{/if}
            {#if totals.files > 0}
              <button class="gp-chip" data-tone="accent" onclick={() => onShowTurn(end.turn)} title="Review what this turn changed">
                {totals.files} {totals.files === 1 ? "file" : "files"}
                <span class="text-ok">+{totals.added}</span>
                <span class="text-danger">−{totals.removed}</span>
              </button>
            {:else if files && !files.available}
              <span>· changes outside edit tools not tracked</span>
            {/if}
            {#if end.summary?.needsAction}
              <span class="w-full text-textPrimary">Next: {end.summary.needsAction}</span>
            {/if}
          </div>
        {/if}
      {/each}
      {#if working && thread.items[thread.items.length - 1]?.kind !== "tool"}
        <div class="mavi-row text-meta text-textMuted animate-pulse" aria-live="polite">{thread.activity ?? "Working"}</div>
      {/if}
    </div>
  </div>
  {#if !pinned}
    <button class="mavi-latest gp-btn" data-variant="secondary" onclick={jump}>
      <ArrowDown size={12} aria-hidden="true" /> Latest
    </button>
  {/if}
</div>

<style>
  .mavi-timeline {
    overflow-anchor: none;
    scrollbar-gutter: stable;
  }
  .mavi-lane {
    max-width: 46rem;
    margin: 0 auto;
    padding: 16px 20px 24px;
  }
  .mavi-row {
    margin-top: 10px;
  }
  .mavi-user-row {
    display: flex;
    justify-content: flex-end;
    margin-top: 20px;
  }
  .mavi-user {
    max-width: 85%;
    padding: 8px 12px;
    border-radius: var(--gp-r-lg);
    background: rgb(var(--c-surface) / 0.9);
    border: 1px solid var(--gp-edge);
  }
  .mavi-turn-end {
    padding-bottom: 6px;
    border-bottom: 1px dashed var(--gp-edge-soft);
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
    box-shadow: var(--shadow-float);
  }
</style>
