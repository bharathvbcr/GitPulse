<script lang="ts">
  /**
   * The Diff view's History tab: the commit graph, in the file rail's column.
   *
   * Opening another commit, or comparing two, no longer means leaving for the
   * Graph view. The lanes are the ones the Graph view solved (the rows come
   * from `graphStore`), painted by the same renderer and compositor at a
   * rail-sized geometry, so the two views cannot disagree about a branch.
   *
   * One scroller: the tab takes the rail's whole height instead of stacking a
   * second list above the file list (see `diff/historyRail.ts` for why that
   * mattered). The canvas sits outside the scroller and is painted at its
   * scroll offset, the arrangement the Graph view uses.
   *
   * Hovering a row (or moving the keyboard cursor onto it) shows the Graph
   * view's commit card, with what the row does not carry read on demand: the
   * message body and the commit's git note (`diff/commitHover.ts`), and its
   * GitPulse provenance from the shared freshness table.
   */
  import { onMount } from "svelte";
  import { GitCompareArrows, Pencil, X } from "@lucide/svelte";
  import Skeleton from "./Skeleton.svelte";
  import {
    GraphRenderer,
    primedRowRange,
    themeFromCss,
    type GraphTheme,
    type VisualCommitRow,
  } from "../canvas/GraphRenderer";
  import { acquireGpu2dContext } from "../canvas/gpuContext";
  import { createGraphStaticCache, createOffscreenSurface } from "../canvas/graphCache";
  import { paintGraphFrame } from "../canvas/graphComposite";
  import { positionGraphTooltip } from "../canvas/graphInteraction";
  import { createFrameScheduler } from "../motion/frameScheduler";
  import { observeResize } from "../dom/observeResize";
  import { watchDevicePixelRatio } from "../dom/devicePixelRatio";
  import { clampScrollTop, computeWindow, ensureNonEmptyWindow } from "../dom/virtualWindow";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
  import { shortHash } from "../format";
  import { timestampFormat } from "../ui/timestampFormat";
  import { themeStore } from "../stores/themeStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { authorCountFor, type RefItem } from "../graph/graphDecorations";
  import type { CommitHoverInfo } from "../diff/commitHover";
  import { freshnessBadge } from "../provenance/badge";
  import type { ProvenanceFreshness } from "../provenance/types";
  import {
    HISTORY_ROW_HEIGHT,
    RAIL_GRAPH_CONFIG,
    commitLabel,
    compareLabel,
    gutterLayout,
    indexOfCommit,
    resolveRowAction,
    sameCommit,
    shouldLoadMore,
    stepRow,
  } from "../diff/historyRail";
  import GraphNodeTooltip from "./GraphNodeTooltip.svelte";
  import FreshnessBadge from "./FreshnessBadge.svelte";
  import MarkdownBody from "./MarkdownBody.svelte";

  let {
    rows,
    refsByCommit,
    closeTargets,
    authorCounts,
    headId = null,
    mainlineName = null,
    hasMore = false,
    loadingMore = false,
    loadError = null,
    selectedCommitId,
    compareBase = null,
    workingTreeCount,
    width,
    hover,
    freshness,
    onHover,
    onVisible,
    onLoadMore,
    onOpenCommit,
    onCompare,
    onExitCompare,
    onPickWorkingTree,
  }: {
    /** Solved graph rows, newest first, exactly as the Graph view drew them. */
    rows: VisualCommitRow[];
    refsByCommit: ReadonlyMap<string, RefItem[]>;
    closeTargets: ReadonlyMap<string, VisualCommitRow>;
    authorCounts: ReadonlyMap<string, number>;
    headId?: string | null;
    mainlineName?: string | null;
    /** Older history exists past the loaded rows. */
    hasMore?: boolean;
    loadingMore?: boolean;
    /** Why the last page of older history failed to load, when it did. */
    loadError?: string | null;
    /** The commit on screen (a comparison's target), or null for uncommitted work. */
    selectedCommitId: string | null;
    /** The base of the comparison on screen, when one is. */
    compareBase?: string | null;
    /** Uncommitted files; -1 when the count is not known. */
    workingTreeCount: number;
    /** The rail's width, which bounds how wide the lane gutter may grow. */
    width: number;
    /** What the hover card reads beyond the row; see `diff/commitHover.ts`. */
    hover: CommitHoverInfo | null;
    /** Provenance per commit id, from the shared freshness table. */
    freshness: Readonly<Record<string, ProvenanceFreshness>>;
    /** The pointer or the cursor rests on a commit (or on nothing). */
    onHover: (commitId: string | null) => void;
    /** The commits now in view, so their provenance can be read in one batch. */
    onVisible: (commitIds: string[]) => void;
    onLoadMore: () => void;
    onOpenCommit: (commitId: string) => void;
    onCompare: (base: string, target: string) => void;
    onExitCompare: () => void;
    onPickWorkingTree: () => void;
  } = $props();

  const ROW = HISTORY_ROW_HEIGHT;
  const OVERSCAN = 8;

  const renderer = new GraphRenderer(RAIL_GRAPH_CONFIG);

  let scroller: HTMLDivElement | undefined = $state();
  let canvas: HTMLCanvasElement | null = $state(null);
  let scrollTop = $state(0);
  let viewportHeight = $state(0);
  /** Keyboard cursor: the row Enter acts on and the card describes. */
  let cursor = $state(-1);
  /** A comparison's first pick, waiting for its second. */
  let armedPick = $state<string | null>(null);
  let pointerRow = $state<number | null>(null);
  let cardAnchor = $state<{ x: number; y: number } | null>(null);
  let cardWidth = $state(320);
  let cardHeight = $state(240);

  const ids = $derived(rows.map((row) => row.id));
  /**
   * The armed pick, while its commit is still in the graph. A reload or a
   * filter that drops it leaves nothing to compare against, so it stops
   * counting (derived, never cleared from an effect that reads it back).
   */
  const armedBase = $derived(
    armedPick !== null && indexOfCommit(ids, armedPick) >= 0 ? armedPick : null,
  );
  const selectedIndex = $derived(indexOfCommit(ids, selectedCommitId));
  const onWorkingTree = $derived(selectedCommitId === null);
  const comparing = $derived(compareBase !== null && selectedCommitId !== null);

  const gutter = $derived(gutterLayout(renderer.measureWidth(rows), width));

  const win = $derived.by(() => {
    const clamped = clampScrollTop(scrollTop, rows.length, ROW, viewportHeight);
    return ensureNonEmptyWindow(
      computeWindow(clamped, viewportHeight, rows.length, ROW, OVERSCAN),
      rows.length,
      ROW,
      viewportHeight,
    );
  });
  const visibleRows = $derived(rows.slice(win.start, win.end));

  /** The row the card describes: the pointer's, else the keyboard cursor's. */
  const cardIndex = $derived(pointerRow ?? (cursor >= 0 ? cursor : null));
  const cardRow = $derived(cardIndex !== null ? (rows[cardIndex] ?? null) : null);
  const cardInfo = $derived(cardRow && hover && hover.commitId === cardRow.id ? hover : null);
  const cardFreshness = $derived(cardRow ? (freshness[cardRow.id] ?? null) : null);
  /** The comparison a click on the card's row would make, in the order it would use. */
  const cardCompare = $derived.by(() => {
    if (!cardRow || armedBase === null) return null;
    const action = resolveRowAction(cardRow.id, armedBase, false, (id) => indexOfCommit(ids, id));
    return action.kind === "compare" ? action : null;
  });
  const cardPosition = $derived(
    cardAnchor
      ? positionGraphTooltip(
          cardAnchor.x,
          cardAnchor.y,
          typeof window === "undefined" ? 1024 : window.innerWidth,
          typeof window === "undefined" ? 768 : window.innerHeight,
          cardWidth,
          cardHeight,
        )
      : null,
  );

  $effect(() => {
    onHover(cardRow?.id ?? null);
  });

  // The visible window's ids, settled: a scroll fling crosses hundreds of
  // rows, and provenance is one batched read for what the reader stopped on.
  let visibleTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const inView = visibleRows.map((row) => row.id);
    if (visibleTimer) clearTimeout(visibleTimer);
    visibleTimer = setTimeout(() => {
      visibleTimer = null;
      if (inView.length > 0) onVisible(inView);
    }, 200);
    return () => {
      if (visibleTimer) clearTimeout(visibleTimer);
      visibleTimer = null;
    };
  });

  // Older history is asked for before the reader reaches the end.
  $effect(() => {
    if (
      shouldLoadMore({
        scrollTop,
        viewportHeight,
        total: rows.length,
        rowHeight: ROW,
        hasMore,
        loading: loadingMore || loadError !== null,
      })
    ) {
      onLoadMore();
    }
  });

  // The selection moved without the rail (the file stepper, another pane):
  // follow it once, never the reader's own scrolling.
  let lastFollowed: string | null = null;
  $effect(() => {
    const id = selectedCommitId;
    const index = selectedIndex;
    if (!id || index < 0 || id === lastFollowed) return;
    lastFollowed = id;
    cursor = index;
    revealRow(index);
  });

  function revealRow(index: number): void {
    if (!scroller || index < 0) return;
    const top = index * ROW;
    const height = scroller.clientHeight;
    if (top < scroller.scrollTop) scroller.scrollTop = top;
    else if (top + ROW > scroller.scrollTop + height) scroller.scrollTop = top + ROW - height;
  }

  function activate(index: number, compareGesture: boolean): void {
    const row = rows[index];
    if (!row) return;
    cursor = index;
    const action = resolveRowAction(row.id, armedBase, compareGesture, (id) => indexOfCommit(ids, id));
    switch (action.kind) {
      case "arm":
        armedPick = action.commitId;
        return;
      case "disarm":
        armedPick = null;
        return;
      case "compare":
        armedPick = null;
        onCompare(action.base, action.target);
        return;
      case "open":
        armedPick = null;
        onOpenCommit(action.commitId);
    }
  }

  function anchorForRow(index: number): { x: number; y: number } | null {
    if (!scroller) return null;
    const rect = scroller.getBoundingClientRect();
    return { x: rect.right, y: rect.top + index * ROW - scroller.scrollTop + ROW / 2 };
  }

  function onRowEnter(index: number): void {
    pointerRow = index;
    cardAnchor = anchorForRow(index);
  }

  function onRowsLeave(): void {
    pointerRow = null;
    cardAnchor = document.activeElement === scroller && cursor >= 0 ? anchorForRow(cursor) : null;
  }

  function onKeydown(event: KeyboardEvent): void {
    // ⌥/⌘/Ctrl chords belong to the Diff view (⌥↑/↓ steps files, ⌥H flips
    // this tab); the list answers only bare keys and Shift.
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "Escape") {
      if (armedBase !== null) {
        event.preventDefault();
        event.stopPropagation();
        armedPick = null;
      } else if (comparing) {
        event.preventDefault();
        event.stopPropagation();
        onExitCompare();
      }
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      if (cursor < 0) return;
      event.preventDefault();
      activate(cursor, event.shiftKey);
      return;
    }
    const pageRows = Math.max(1, Math.floor((scroller?.clientHeight ?? ROW) / ROW) - 1);
    const next = stepRow(cursor, event.key, rows.length, pageRows);
    if (next === null) return;
    event.preventDefault();
    cursor = next;
    pointerRow = null;
    revealRow(next);
    cardAnchor = anchorForRow(next);
  }

  function onFocus(): void {
    if (cursor < 0) cursor = selectedIndex >= 0 ? selectedIndex : 0;
    if (pointerRow === null && cursor >= 0) cardAnchor = anchorForRow(cursor);
  }

  function onBlur(): void {
    if (pointerRow === null) cardAnchor = null;
  }

  function onScroll(): void {
    if (!scroller) return;
    scrollTop = scroller.scrollTop;
    // The card stays with its row, or goes when the row scrolls away.
    if (cardIndex !== null) {
      const anchor = anchorForRow(cardIndex);
      const rect = scroller.getBoundingClientRect();
      cardAnchor = anchor && anchor.y >= rect.top && anchor.y <= rect.bottom ? anchor : null;
    }
    schedulePaint();
  }

  // --- painting ------------------------------------------------------------

  const scheduler = createFrameScheduler();
  let gpuCtx: CanvasRenderingContext2D | null = null;
  let attachedCanvas: HTMLCanvasElement | null = null;
  let cachedTheme: GraphTheme | null = null;
  let rowsVersion = 0;
  let versionedRows: VisualCommitRow[] | null = null;

  // Same strip contract as the Graph view: primed with the lookback so short
  // connectors crossing a strip seam stay whole; long ones are drawn by the
  // compositor's overlay instead.
  const graphCache = createGraphStaticCache((req) => {
    const range = primedRowRange(req.firstRow, req.rowCount, rows.length);
    renderer.render(req.ctx, rows, range.from, range.to, req.stripTopCss, undefined, {
      theme: currentTheme(),
      viewportHeight: req.viewportCssHeight,
      skipLongConnectors: true,
    });
  }, createOffscreenSurface);

  function currentTheme(): GraphTheme {
    // Transparent, so the rail's own surface shows through the gutter.
    if (!cachedTheme) cachedTheme = { ...themeFromCss(canvas), background: null };
    return cachedTheme;
  }

  function currentRowsVersion(): number {
    if (rows !== versionedRows) {
      versionedRows = rows;
      rowsVersion += 1;
    }
    return rowsVersion;
  }

  function ensureContext(): CanvasRenderingContext2D | null {
    if (!canvas) return null;
    if (attachedCanvas !== canvas) {
      attachedCanvas = canvas;
      gpuCtx = acquireGpu2dContext(canvas, false);
    }
    return gpuCtx;
  }

  function paintNow(): void {
    const ctx = ensureContext();
    if (!ctx || !canvas || viewportHeight <= 0) return;
    paintGraphFrame(ctx, canvas, renderer, graphCache, {
      rows,
      dataVersion: currentRowsVersion(),
      widthCss: gutter.width,
      heightCss: viewportHeight,
      dpr: window.devicePixelRatio || 1,
      densitySignature: "rail",
      theme: currentTheme(),
      scrollTop: clampScrollTop(scrollTop, rows.length, ROW, viewportHeight),
      startIndex: win.start,
      endIndex: win.end,
      selectedCommitId: selectedCommitId,
      headCommitId: headId,
      hoveredCommitId: cardRow?.id ?? null,
      hoverStrength: 1,
      selectionStrength: 1,
    });
  }

  function schedulePaint(): void {
    scheduler.schedule(() => paintNow());
  }

  // Anything the frame reads repaints it.
  $effect(() => {
    rows;
    gutter.width;
    viewportHeight;
    selectedCommitId;
    headId;
    cardRow;
    schedulePaint();
  });

  const accent = $derived($interfaceStore.accent);
  $effect(() => {
    $themeStore;
    accent;
    cachedTheme = null;
    graphCache.invalidate();
    schedulePaint();
  });

  onMount(() => {
    const stopResize = scroller
      ? observeResize(scroller, (entries) => {
          const measured = entries[0]?.contentRect.height;
          viewportHeight = Number.isFinite(measured) && measured > 0 ? measured : 0;
        })
      : () => {};
    const stopDpr = watchDevicePixelRatio(() => {
      graphCache.invalidate();
      schedulePaint();
    });
    const lost = () => {
      gpuCtx = null;
      attachedCanvas = null;
    };
    const restored = () => {
      graphCache.invalidate();
      schedulePaint();
    };
    canvas?.addEventListener("contextlost", lost);
    canvas?.addEventListener("contextrestored", restored);
    if (selectedIndex >= 0) revealRow(selectedIndex);
    return () => {
      stopResize();
      stopDpr();
      canvas?.removeEventListener("contextlost", lost);
      canvas?.removeEventListener("contextrestored", restored);
      scheduler.cancel();
      graphCache.dispose();
      onHover(null);
    };
  });

  function rowRefs(row: VisualCommitRow): RefItem[] {
    return refsByCommit.get(row.id) ?? [];
  }

  function provenanceDetail(f: ProvenanceFreshness): string {
    return freshnessBadge(f).detail;
  }
</script>

<div class="flex min-h-0 flex-1 flex-col" data-diff-history>
  <!-- Uncommitted work first: it is what a reader most often comes back to,
       and the one change that is not in the graph. -->
  <button
    type="button"
    class="flex w-full shrink-0 items-center gap-1.5 px-2.5 py-1 text-left text-meta hover:bg-surfaceHover {onWorkingTree
      ? 'bg-accent/10 text-accent'
      : 'text-textMuted'}"
    aria-current={onWorkingTree ? "true" : undefined}
    onclick={() => {
      armedPick = null;
      onPickWorkingTree();
    }}
  >
    <Pencil size={11} class="shrink-0" />
    <span class="min-w-0 flex-1 truncate">Uncommitted changes</span>
    <!-- -1 is "not counted"; rendering it as 0 would report an unscanned
         working tree as clean. -->
    {#if workingTreeCount > 0}
      <span class="shrink-0 font-mono text-micro">{workingTreeCount}</span>
    {:else if workingTreeCount === 0}
      <span class="shrink-0 text-micro">clean</span>
    {/if}
  </button>

  {#if armedBase}
    <div
      class="flex shrink-0 items-center gap-1.5 bg-accent/10 px-2.5 py-1 text-meta text-accent"
      role="status"
      data-compare-armed
    >
      <GitCompareArrows size={11} class="shrink-0" />
      <span class="min-w-0 flex-1 truncate">
        Comparing from <span class="font-mono">{shortHash(armedBase)}</span> — pick the other commit
      </span>
      <button
        type="button"
        class="gp-icon-btn"
        data-size="sm"
        title="Cancel the comparison (Esc)"
        aria-label="Cancel the comparison"
        onclick={() => (armedPick = null)}
      >
        <X size={11} />
      </button>
    </div>
  {/if}

  {#if gutter.clipped}
    <!-- The lanes furthest from the mainline are cut, and the reader is told
         where the rest is, rather than lanes silently ending at an edge. -->
    <p class="shrink-0 px-2.5 py-0.5 text-micro text-textMuted" data-graph-clipped>
      Some branches are cut off — widen the rail or open Graph to see them all.
    </p>
  {/if}

  {#if rows.length === 0}
    <p class="px-2.5 py-3 text-meta text-textMuted">No commits loaded yet.</p>
  {/if}

  <div class="relative min-h-0 flex-1" class:hidden={rows.length === 0}>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- Justified: this is the ARIA listbox pattern with
         aria-activedescendant — the list itself takes focus and moves a
         cursor over its options on the arrow keys; svelte-check classifies a
         focusable listbox container as non-interactive. -->

    <div
      bind:this={scroller}
      class="h-full overflow-y-auto gp-scroll outline-hidden focus-visible:ring-1 focus-visible:ring-accent/60"
      role="listbox"
      tabindex="0"
      aria-label="Commit history — Enter opens a commit, Shift+Enter starts a comparison"
      aria-activedescendant={cursor >= 0 && rows[cursor] ? `diff-history-${rows[cursor].id}` : undefined}
      onscroll={onScroll}
      onkeydown={onKeydown}
      onfocus={onFocus}
      onblur={onBlur}
      onpointerleave={onRowsLeave}
    >
      <div style="height: {rows.length * ROW}px; position: relative;">
        <div class="absolute inset-x-0 top-0" style="transform: translate3d(0, {win.start * ROW}px, 0);">
          {#each visibleRows as row, i (row.id)}
            {@const index = win.start + i}
            {@const active = index === selectedIndex}
            {@const isBase = compareBase !== null && sameCommit(row.id, compareBase)}
            {@const armed = armedBase === row.id}
            {@const refs = rowRefs(row)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <!-- Justified: the keyboard path is the listbox's own onkeydown
                 (Enter opens the option under the cursor, Shift+Enter starts
                 a comparison); an option is never focused itself, so a key
                 handler on it would never run. -->

            <div
              id="diff-history-{row.id}"
              role="option"
              tabindex="-1"
              aria-selected={active}
              data-commit-id={row.id}
              class="group flex cursor-pointer items-center gap-1 pr-1 text-meta {active
                ? 'bg-accent/10 text-accent'
                : isBase || armed
                  ? 'bg-accent/5 text-textPrimary'
                  : 'text-textMuted hover:bg-surfaceHover'} {index === cursor ? 'ring-1 ring-inset ring-accent/40' : ''}"
              style="height: {ROW}px; padding-left: {gutter.width}px"
              onpointerenter={() => onRowEnter(index)}
              onclick={(event) => activate(index, event.shiftKey)}
            >
              {#if isBase || armed}
                <span class="shrink-0 rounded bg-accent/15 px-1 font-mono text-micro text-accent">base</span>
              {/if}
              {#if refs.length > 0}
                <span
                  class="gp-chip max-w-20 shrink-0 font-mono"
                  data-tone="neutral"
                  title={refs.map((ref) => ref.name).join(", ")}
                >{refs[0].name}{refs.length > 1 ? ` +${refs.length - 1}` : ""}</span>
              {/if}
              <span class="min-w-0 flex-1 truncate">{commitLabel(row.summary)}</span>
              <!-- What the lanes and the card show, in words: the canvas is
                   aria-hidden, so a merge must not be a shape only. -->
              <span class="sr-only">
                , {shortHash(row.id)} by {row.author_name || "unknown"}{row.is_merge ? ", merge commit" : ""}{active
                  ? ", on screen"
                  : ""}
              </span>
              <span class="shrink-0 font-mono text-micro opacity-60 group-hover:hidden">
                {$timestampFormat.text(row.timestamp)}
              </span>
              <button
                type="button"
                class="gp-icon-btn hidden group-hover:inline-flex {armed ? 'text-accent' : ''}"
                data-size="sm"
                title={armedBase && !armed
                  ? "Compare with the commit picked first"
                  : armed
                    ? "Cancel the comparison"
                    : "Compare from this commit (Shift+click)"}
                aria-label={armedBase && !armed ? "Compare with the first pick" : "Compare from this commit"}
                data-compare-button
                onclick={(event) => {
                  event.stopPropagation();
                  activate(index, armedBase === null || armed);
                }}
              >
                <GitCompareArrows size={11} />
              </button>
            </div>
          {/each}
        </div>
      </div>

      {#if loadError}
        <div class="flex items-center gap-2 px-2.5 py-1.5 text-meta text-warn" role="status">
          <span class="min-w-0 flex-1 truncate" title={loadError}>Couldn't load older history. {loadError}</span>
          <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={onLoadMore}>Try again</button>
        </div>
      {:else if loadingMore}
        <p class="px-2.5 py-1.5 text-meta text-textMuted" role="status">Loading older history…</p>
        <Skeleton variant="tree-row" count={2} hold={false} />
      {:else if !hasMore && rows.length > 0}
        <p class="px-2.5 py-1.5 text-micro text-textMuted">Start of the loaded history.</p>
      {/if}
    </div>
    <canvas
      bind:this={canvas}
      class="pointer-events-none absolute left-0 top-0 block"
      style="width: {gutter.width}px; height: {viewportHeight}px;"
      aria-hidden="true"
    ></canvas>
  </div>
</div>

{#if cardRow && cardPosition}
  <!-- Portaled and fixed: the rail clips its overflow, and a card cut at the
       rail's edge would hide exactly the part beside the diff. Pointer cards
       are aria-hidden (the row's own text is what a screen reader reads); the
       keyboard cursor's card is that user's UI, so it stays exposed. -->
  <div
    use:portal={"body"}
    bind:clientWidth={cardWidth}
    bind:clientHeight={cardHeight}
    aria-hidden={pointerRow !== null ? true : undefined}
    class="pointer-events-none fixed left-0 top-0 w-80 max-w-[calc(100vw-1rem)]"
    style="transform: translate3d({cardPosition.left}px, {cardPosition.top}px, 0); z-index: {LAYERS.TOOLTIP};"
    data-diff-history-card
  >
    <GraphNodeTooltip
      row={cardRow}
      refs={rowRefs(cardRow)}
      placement={cardPosition.placement}
      caretX={cardPosition.anchorX}
      mergeTarget={closeTargets.get(cardRow.id) ?? null}
      authorCommitCount={authorCountFor(authorCounts, cardRow)}
      {mainlineName}
      {hasMore}
    >
      <div class="max-h-56 overflow-hidden px-3 pb-2.5 text-meta" data-card-details>
        {#if !cardInfo || cardInfo.body.state === "loading"}
          <p class="text-textMuted">Reading the commit message…</p>
        {:else if cardInfo.body.state === "error"}
          <p class="text-warn">Couldn't read the message body. {cardInfo.body.error}</p>
        {:else if cardInfo.body.value.trim().length > 0}
          <div class="max-h-28 overflow-hidden text-textPrimary" data-card-body>
            <MarkdownBody source={cardInfo.body.value} />
          </div>
        {:else}
          <p class="text-textMuted">No message body.</p>
        {/if}

        {#if cardInfo && cardInfo.note.state === "ready" && cardInfo.note.value !== null}
          <div class="mt-2 rounded-lg bg-background px-2 py-1" data-card-note>
            <div class="gp-label mb-0.5">Note</div>
            <p class="whitespace-pre-wrap break-words text-textPrimary">{cardInfo.note.value}</p>
          </div>
        {:else if cardInfo && cardInfo.note.state === "error"}
          <p class="mt-2 text-warn" data-card-note-error>
            Couldn't read the notes. {cardInfo.note.error}
          </p>
        {/if}

        {#if cardFreshness}
          <div class="mt-2 flex items-start gap-1.5" data-card-provenance>
            <FreshnessBadge freshness={cardFreshness} />
            <span class="min-w-0 flex-1 text-textMuted">{provenanceDetail(cardFreshness)}</span>
          </div>
          {#if cardFreshness.session?.summary}
            <p class="mt-1 text-textMuted">
              Session ({cardFreshness.session.actor_kind}): {cardFreshness.session.summary}
            </p>
          {/if}
        {/if}

        <p class="mt-2 text-micro text-textMuted">
          {#if cardCompare}
            Click to compare <span class="font-mono">{compareLabel(cardCompare.base, cardCompare.target)}</span>
          {:else}
            Click to open · Shift+click to compare
          {/if}
        </p>
      </div>
    </GraphNodeTooltip>
  </div>
{/if}
