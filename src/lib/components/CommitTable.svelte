<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { graphStore } from "../stores/graphStore";
  import { repoStore } from "../stores/repoStore";
  import { filterStore } from "../stores/filterStore";
  import { themeStore } from "../stores/themeStore";
  import { densityStore } from "../stores/densityStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import {
    AVATAR_STYLES,
    DENSITY_CONFIGS,
    GraphRenderer,
    primedRowRange,
    themeFromCss,
    type DensityMode,
    type GraphTheme,
    type VisualCommitRow,
  } from "../canvas/GraphRenderer";
  import { acquireGpu2dContext } from "../canvas/gpuContext";
  import { diagnostics } from "../diagnostics/diagnostics";
  import {
    createGraphStaticCache,
    createOffscreenSurface,
  } from "../canvas/graphCache";
  import { paintGraphFrame } from "../canvas/graphComposite";
  import { createFrameScheduler } from "../motion/frameScheduler";
  import { prefersReducedMotion } from "../motion/easing";
  import { INITIAL_GRAPH_PAINT, stepGraphPaint, type GraphPaintState } from "../motion/graphPaint";
  import { createRowFilterMemo, parseFilterQueryCached } from "../filter/queryMemo";
  import { nextLoadLimit } from "../stores/graphLimits";
  import {
    forwardGraphWheel,
    positionGraphTooltip,
    type TooltipPlacement,
  } from "../canvas/graphInteraction";
  import CommitRow, { type RefItem } from "./CommitRow.svelte";
  import GraphNodeTooltip from "./GraphNodeTooltip.svelte";

  let isLoadingMore = $state(false);

  let loadMoreLabel = $derived.by(() => {
    const next = nextLoadLimit($graphStore.maxCommits);
    if (next === null) return "All loaded history shown";
    return `Load older history (${next.toLocaleString()} commits)`;
  });

  async function loadMore() {
    const path = $repoStore.currentPath;
    if (!path || isLoadingMore) return;
    isLoadingMore = true;
    try {
      await graphStore.loadMore(path, $filterStore.searchQuery, $filterStore.selectedBranch);
    } finally {
      isLoadingMore = false;
    }
  }

  let root: HTMLDivElement | null = $state(null);
  let container: HTMLDivElement | null = $state(null);
  let canvas: HTMLCanvasElement | null = $state(null);
  let scrollTop = $state(0);
  let containerHeight = $state(600);

  const renderer = new GraphRenderer(DENSITY_CONFIGS[get(densityStore)]);
  const scheduler = createFrameScheduler();

  let rowHeight = $derived(DENSITY_CONFIGS[$densityStore].rowHeight);

  let gpuCtx: CanvasRenderingContext2D | null = null;
  let attachedCanvas: HTMLCanvasElement | null = null;
  let hoveredCommitId: string | null = null;
  let tooltipRow: (typeof filteredRows)[number] | null = $state(null);
  let tooltipLeft = $state(0);
  let tooltipTop = $state(0);
  let tooltipAnchorX = $state(16);
  let tooltipPlacement: TooltipPlacement = $state("below");
  // Measured box of the rendered tooltip; the old hardcoded 320×190 lied
  // about real height (wrapped summaries, ref-chip rows), so "fits below"
  // could pin a taller box against the pane bottom with its caret adrift.
  let tooltipBoxWidth = $state(320);
  let tooltipBoxHeight = $state(190);
  let tooltipPointer: { x: number; y: number } | null = null;
  let paintState: GraphPaintState = { ...INITIAL_GRAPH_PAINT };
  let lastSelectedId: string | null = null;
  let selectionSeen = false;
  let lastFrameTime = 0;

  // Theme is resolved from the stylesheet once per theme change, never inside
  // the per-frame paint path (getComputedStyle every frame thrashes style).
  let cachedTheme: GraphTheme | null = null;
  // Cached layout rects: getBoundingClientRect at pointer frequency forces
  // synchronous layout; they are re-read only after scroll/resize/theme flips.
  let gutterRect: DOMRect | null = null;
  let rootRect: DOMRect | null = null;
  // Pointer events are coalesced: the latest position wins, one hit test/frame.
  let pendingPointer: { x: number; y: number } | null = null;
  // Last seen cursor position, kept so a scroll can RE-hit instead of
  // killing the tooltip (wheel-over-gutter forwards here; a null→remount
  // cycle replayed the tooltip entrance on every scroll frame).
  let lastPointer: { x: number; y: number } | null = null;
  // Row-data version for the static-layer cache key: bumps when the filtered
  // array identity changes (new payload, filter edit, repo switch).
  let rowsVersion = 0;
  let versionedRows: VisualCommitRow[] | null = null;

  // Identity-stable filtering: without the memo, every graphStore emission
  // handed derivations a fresh array and bumped rowsVersion, wiping the
  // strip cache (full re-rasterization) even when history was unchanged.
  const rowFilter = createRowFilterMemo();

  let filtered = $derived.by(() =>
    rowFilter.filter($graphStore.rows, parseFilterQueryCached($filterStore.searchQuery))
  );
  let filteredRows = $derived(filtered.rows);

  /**
   * Author-avatar column (Settings → Commit Graph → "Author avatars").
   *
   * The column sits right of the lanes: rows align by node Y, so authorship
   * stays attributable at a glance without ever colliding with dense lane
   * traffic. The caller owns its geometry — the renderer only receives the
   * column centre X.
   */
  let showAvatars = $derived($interfaceStore.showGraphAvatars);
  let densityMode = $derived<DensityMode>($densityStore);
  let avatarSlot = $derived.by(() => {
    if (!showAvatars) return { gap: 0, radius: 0, width: 0 };
    const style = AVATAR_STYLES[densityMode];
    const gap = densityMode === "spacious" ? 12 : 9;
    return { gap, radius: style.radius, width: gap + style.radius * 2 };
  });

  // Single owner of gutter math (GraphRenderer.measureWidth): it counts node
  // lanes, active lanes AND connection target lanes, so a dangling merged-in
  // branch's fan-out column can never clip under a hand-rolled formula. The
  // avatar slot rides on top so toggling avatars resizes the gutter once.
  let laneAreaWidth = $derived(Math.max(220, renderer.measureWidth(filteredRows)));
  let graphColumnWidth = $derived(laneAreaWidth + avatarSlot.width);
  /** Centre X of the avatar column; null when the option is off. */
  let avatarCenterX = $derived(
    showAvatars && avatarSlot.width > 0
      ? laneAreaWidth - renderer.getConfig().originX + avatarSlot.gap + avatarSlot.radius
      : null,
  );

  /**
   * Ref chips, taken from the graph payload rather than derived from the
   * branch list.
   *
   * The backend resolves them in one `for-each-ref` pass, which is the only
   * place two cases are answered correctly: an annotated tag has to be peeled
   * to the commit it points at before it can decorate a row, and a detached
   * HEAD belongs to no branch tip at all. A derivation from `branches` shows
   * the first on no row and the second nowhere.
   */
  let refsByCommit = $derived.by(() => {
    const map = new Map<string, RefItem[]>();
    for (const ref of $graphStore.refs) {
      const kind: RefItem["kind"] =
        ref.kind === "head"
          ? "head"
          : ref.kind === "tag"
            ? "tag"
            : ref.kind === "remote"
              ? "remote-branch"
              : ref.is_head
                ? "current-branch"
                : "local-branch";
      const list = map.get(ref.commit_id) ?? [];
      if (!list.some((r) => r.name === ref.name && r.kind === kind)) {
        list.push({ name: ref.name, kind });
      }
      map.set(ref.commit_id, list);
    }
    return map;
  });

  let totalCommits = $derived(filteredRows.length);
  let startIndex = $derived(Math.max(0, Math.floor(scrollTop / rowHeight) - 5));
  let visibleCount = $derived(Math.ceil(containerHeight / rowHeight) + 10);
  let endIndex = $derived(Math.min(totalCommits, startIndex + visibleCount));
  let visibleRows = $derived(filteredRows.slice(startIndex, endIndex));

  /**
   * Offscreen static layer: connectors, nodes and decorations are tile-rendered
   * once per data/width/density/theme/dpr change; scroll frames just blit the
   * visible strips and stroke the live emphasis rings on top.
   */
  const graphCache = createGraphStaticCache(
    (req) => {
      // Prime with LOOKBACK_ROWS so short connectors crossing strip seams stay
      // continuous; render() culls to the strip's own height. Long connectors
      // are excluded here on purpose — paintGraphFrame draws them whole in the
      // live overlay, since a span beyond the primed lookback cannot be baked
      // into a tile without a mid-edge seam clip.
      const range = primedRowRange(req.firstRow, req.rowCount, filteredRows.length);
      renderer.render(
        req.ctx,
        filteredRows,
        range.from,
        range.to,
        req.stripTopCss,
        undefined,
        {
          theme: currentTheme(),
          viewportHeight: req.viewportCssHeight,
          skipLongConnectors: true,
          showAvatars,
          avatarX: avatarCenterX,
        },
        null,
      );
    },
    createOffscreenSurface,
  );

  function currentTheme(): GraphTheme {
    if (!cachedTheme) cachedTheme = themeFromCss(canvas);
    return cachedTheme;
  }

  function currentRowsVersion(): number {
    if (filteredRows !== versionedRows) {
      versionedRows = filteredRows;
      rowsVersion += 1;
    }
    return rowsVersion;
  }

  function canvasRect(): DOMRect | null {
    if (!canvas) return null;
    if (!gutterRect) gutterRect = canvas.getBoundingClientRect();
    return gutterRect;
  }

  function ensureContext(): CanvasRenderingContext2D | null {
    if (!canvas) return null;
    if (attachedCanvas !== canvas) {
      attachedCanvas = canvas;
      gpuCtx = acquireGpu2dContext(canvas, true);
    }
    return gpuCtx;
  }

  function selectedId(): string | null {
    return $repoStore.selectedCommitId || $graphStore.selectedCommit?.id || null;
  }

  function paintNow() {
    const ctx = ensureContext();
    if (!ctx || !canvas) return;

    // Layering (background → strip blits → dangling stubs → emphasis rings)
    // lives in paintGraphFrame; see graphComposite.ts.
    paintGraphFrame(ctx, canvas, renderer, graphCache, {
      rows: filteredRows,
      dataVersion: currentRowsVersion(),
      widthCss: graphColumnWidth,
      heightCss: containerHeight,
      dpr: window.devicePixelRatio || 1,
      densitySignature: $densityStore,
      theme: currentTheme(),
      scrollTop,
      startIndex,
      endIndex,
      selectedCommitId: selectedId(),
      headCommitId: $graphStore.headId,
      hoveredCommitId: paintState.displayHoverId,
      hoverStrength: paintState.hoverStrength,
      selectionStrength: paintState.selectionStrength,
      showAvatars,
      avatarX: avatarCenterX,
    });
  }

  function onFrame(now: number) {
    const dt = lastFrameTime ? Math.min(32, now - lastFrameTime) : 16;
    lastFrameTime = now;
    const pointer = pendingPointer;
    pendingPointer = null;
    if (pointer) processPointer(pointer.x, pointer.y);

    const currentSelected = selectedId();
    // The first observed frame only LEARNS the selection; treating it as a
    // change played a phantom grow-in pulse on every mount and repo switch.
    const selectionReset = selectionSeen && currentSelected !== lastSelectedId;
    selectionSeen = true;
    lastSelectedId = currentSelected;

    const { next, animating } = stepGraphPaint(paintState, {
      hoveredCommitId,
      selectionReset,
      deltaMs: dt,
      reducedMotion: prefersReducedMotion(),
    });
    paintState = next;
    paintNow();
    if (animating) scheduler.schedule(onFrame);
  }

  function schedulePaint() {
    scheduler.schedule(onFrame);
  }

  function handleScroll(e: Event) {
    const target = e.target as HTMLDivElement;
    scrollTop = target.scrollTop;
    gutterRect = null;
    if (lastPointer) {
      pendingPointer = lastPointer;
    } else {
      tooltipRow = null;
      hoveredCommitId = null;
    }
    schedulePaint();
  }

  function handleGraphWheel(e: WheelEvent) {
    if (!container) return;
    // Trackpad pinch arrives as wheel+ctrlKey. There is no graph zoom to
    // forward it to, so it must be consumed here: letting it through makes
    // the webview zoom the whole app while the pointer is over the gutter —
    // the primary place users pinch by accident.
    if (e.ctrlKey) {
      e.preventDefault();
      return;
    }
    const moved = forwardGraphWheel(
      container,
      e.deltaY,
      e.deltaMode,
      rowHeight,
    );
    if (moved) e.preventDefault();
  }

  function hitAt(clientX: number, clientY: number) {
    const rect = canvasRect();
    if (!rect) return null;
    return renderer.getCommitAtPoint(
      clientX - rect.left,
      clientY - rect.top,
      filteredRows,
      startIndex,
      endIndex,
      scrollTop,
      showAvatars ? avatarCenterX : null,
    );
  }

  function handleCanvasClick(e: MouseEvent) {
    const hit = hitAt(e.clientX, e.clientY);
    if (hit) {
      graphStore.selectCommit(hit);
      repoStore.selectCommitDiff(hit.id);
    }
  }

  /**
   * Pointer moves only record the latest position; the frame callback below
   * performs at most one hit test per vsync (latest-wins).
   */
  function handleCanvasMouseMove(e: MouseEvent) {
    lastPointer = { x: e.clientX, y: e.clientY };
    pendingPointer = lastPointer;
    schedulePaint();
  }

  function processPointer(x: number, y: number) {
    const hit = hitAt(x, y);
    const nextId = hit ? hit.id : null;
    if (canvas) canvas.style.cursor = nextId ? "pointer" : "default";
    hoveredCommitId = nextId;
    if (!hit || !root) {
      tooltipRow = null;
      tooltipPointer = null;
      return;
    }
    tooltipPointer = { x, y };
    tooltipRow = hit;
    placeTooltip(x, y);
  }

  /** Positions the open tooltip from its MEASURED box, not assumed metrics. */
  function placeTooltip(pointerX: number, pointerY: number) {
    if (!root) return;
    if (!rootRect) rootRect = root.getBoundingClientRect();
    const position = positionGraphTooltip(
      pointerX - rootRect.left,
      pointerY - rootRect.top,
      root.clientWidth,
      root.clientHeight,
      Math.max(32, tooltipBoxWidth),
      Math.max(32, tooltipBoxHeight),
    );
    tooltipLeft = position.left;
    tooltipTop = position.top;
    tooltipAnchorX = position.anchorX;
    tooltipPlacement = position.placement;
  }

  // Late measurement re-placement: the box bindings land a frame after the
  // tooltip mounts, so the first placement used fallback metrics. Re-run the
  // placement with real ones (and on every resize of the box thereafter).
  $effect(() => {
    void tooltipBoxWidth;
    void tooltipBoxHeight;
    const pointer = tooltipPointer;
    if (tooltipRow && pointer) placeTooltip(pointer.x, pointer.y);
  });

  function handleCanvasMouseLeave() {
    if (canvas) canvas.style.cursor = "default";
    pendingPointer = null;
    lastPointer = null;
    if (hoveredCommitId !== null) {
      hoveredCommitId = null;
      schedulePaint();
    }
    tooltipRow = null;
  }

  $effect(() => {
    filteredRows;
    startIndex;
    endIndex;
    containerHeight;
    graphColumnWidth;
    $repoStore.selectedCommitId;
    // The canvas ring must not wait on the async diff round-trip: selecting
    // updates graphStore synchronously, repoStore.selectedCommitId only after
    // the diff resolves (or never, when it fails). Both are dependencies.
    $graphStore.selectedCommit;
    $graphStore.headId;
    $densityStore;
    $interfaceStore.showGraphAvatars;
    renderer.setDensity($densityStore);
    // Density flips change the gutter width and can shift layout around it;
    // the cached pointer rects must not survive the shift or hit-testing
    // lands offset by exactly whatever moved. Same contract as the theme
    // effect below. The avatar toggle rides along: it resizes the gutter too.
    gutterRect = null;
    rootRect = null;
    // A filter edit or payload refresh can swap `filteredRows` while the
    // pointer rests over the gutter: mouseleave will never fire, so a tooltip
    // rendered from the PREVIOUS array would keep floating (possibly showing
    // a commit the new filter excludes). Re-validate instead of blindly
    // clearing, so a genuine re-hit under a stationary cursor stays put.
    if (tooltipRow && !filteredRows.includes(tooltipRow)) {
      tooltipRow = null;
      hoveredCommitId = null;
    }
    schedulePaint();
  });

  // Theme changes are rare; drop everything keyed to the old palette and let
  // paintNow re-resolve the stylesheet at frame time. The store may flip the
  // html class inside a view-transition callback — i.e. after this effect
  // runs — so reading getComputedStyle here could cache the previous theme.
  $effect(() => {
    $themeStore;
    cachedTheme = null;
    gutterRect = null;
    rootRect = null;
    graphCache.invalidate();
    schedulePaint();
  });

  onMount(() => {
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      // Distinguish "no measurement" (keep the previous height) from a real
      // 0px measurement (collapsed pane): `|| 600` used to turn a genuine
      // zero into a phantom viewport and over-render invisible rows.
      if (!entry) return;
      containerHeight = entry.contentRect.height || 0;
      gutterRect = null;
      rootRect = null;
    });
    if (container) observer.observe(container);

    // Display moves change devicePixelRatio without any resize event; watch the
    // current resolution and re-arm on each transition.
    let detachDpr = () => {};
    const watchDpr = () => {
      if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
      const query = `(resolution: ${window.devicePixelRatio || 1}dppx)`;
      const media = window.matchMedia(query);
      const onChange = () => {
        detachDpr();
        watchDpr();
        graphCache.invalidate();
        schedulePaint();
      };
      if (typeof media.addEventListener === "function") {
        media.addEventListener("change", onChange);
        detachDpr = () => media.removeEventListener("change", onChange);
      }
    };
    watchDpr();

    // GPU pressure can revoke the 2D context mid-session; without handling,
    // every subsequent paint silently no-ops into the dead context and the
    // graph freezes. Dropping the cached context + geometry forces
    // ensureContext() to re-acquire (the browser fires contextrestored once
    // the context is usable again; until then paints stay cheap no-ops).
    const onContextLost = (event: Event) => {
      event.preventDefault();
      gpuCtx = null;
      attachedCanvas = null;
      gutterRect = null;
      rootRect = null;
      graphCache.invalidate();
      schedulePaint();
      diagnostics.warn("canvas", "2D context lost; re-acquiring on next paint");
    };
    const onContextRestored = () => {
      graphCache.invalidate();
      schedulePaint();
      diagnostics.warn("canvas", "2D context restored");
    };
    canvas?.addEventListener("contextlost", onContextLost);
    canvas?.addEventListener("contextrestored", onContextRestored);

    return () => {
      detachDpr();
      canvas?.removeEventListener("contextlost", onContextLost);
      canvas?.removeEventListener("contextrestored", onContextRestored);
      observer.disconnect();
      scheduler.cancel();
      graphCache.dispose();
    };
  });
</script>

<div bind:this={root} class="flex-1 flex overflow-hidden relative bg-background">
  <!-- Loading and error states live above the panes: a graph load on a huge
       repository is seconds of work, and "nothing happened" is not an
       acceptable answer for it. -->
  {#if $graphStore.isLoading}
    <div class="absolute top-0 inset-x-0 h-0.5 z-30 overflow-hidden">
      <div class="h-full w-1/3 bg-accent animate-[gp-slide_1.2s_ease-in-out_infinite]"></div>
    </div>
  {/if}
  {#if $graphStore.error && !$graphStore.isLoading}
    <div class="absolute top-1 left-1/2 -translate-x-1/2 z-30 px-3 py-1.5 rounded-lg bg-surface border border-rose-500/40 text-[11px] text-rose-300 shadow-lg flex items-center gap-2 max-w-[90%]">
      <span class="truncate" title={$graphStore.error}>{$graphStore.error}</span>
      <button
        class="shrink-0 underline hover:text-white"
        onclick={() => {
          const path = $repoStore.currentPath;
          if (path) void graphStore.loadGraph(path, $filterStore.searchQuery, $filterStore.selectedBranch);
        }}
      >
        Retry
      </button>
    </div>
  {/if}

  <div
    class="shrink-0 border-r border-border/40 relative z-10 bg-background gp-gpu cursor-pointer"
    style="width: {graphColumnWidth}px;"
    onclick={handleCanvasClick}
    onmousemove={handleCanvasMouseMove}
    onmouseleave={handleCanvasMouseLeave}
    onwheel={handleGraphWheel}
    role="presentation"
  >
    <canvas bind:this={canvas} class="gp-gpu w-full h-full block"></canvas>
  </div>

  <div
    bind:this={container}
    onscroll={handleScroll}
    class="flex-1 overflow-y-auto overflow-x-hidden relative gp-scroll"
  >
    <div style="height: {totalCommits * rowHeight}px; width: 100%; position: relative;">
      <div
        class="gp-gpu"
        style="transform: translate3d(0, {startIndex * rowHeight}px, 0); width: 100%;"
      >
        {#each visibleRows as row (row.id)}
          <CommitRow
            {row}
            density={$densityStore}
            refs={refsByCommit.get(row.id) ?? []}
            isSelected={$repoStore.selectedCommitId === row.id}
            onSelect={() => {
              graphStore.selectCommit(row);
              repoStore.selectCommitDiff(row.id);
            }}
          />
        {/each}
      </div>
    </div>
    {#if totalCommits === 0 && !$graphStore.isLoading && !$graphStore.error}
      <div class="absolute inset-0 flex items-center justify-center text-textMuted text-xs pointer-events-none">
        {#if $filterStore.searchQuery.trim() === "" && !$filterStore.selectedBranch}
          This repository has no commits yet.
        {:else}
          No commits match the current filters.
        {/if}
      </div>
    {/if}
    {#if $graphStore.hasMore}
      <button
        onclick={loadMore}
        disabled={isLoadingMore || nextLoadLimit($graphStore.maxCommits) === null}
        class="block mx-auto my-2 px-3 py-1.5 rounded-lg border border-border bg-surface text-[11px] text-textMuted hover:text-textPrimary hover:border-accent/60 transition-colors disabled:opacity-50"
      >
        {#if isLoadingMore}
          <span class="inline-block animate-spin mr-1.5">◌</span>Loading older history…
        {:else}
          {loadMoreLabel}
        {/if}
      </button>
    {/if}
  </div>

  {#if tooltipRow}
    <!-- Positioned via transform: left/top writes invalidate layout at pointer
         frequency; a composited translate3d does not. clientWidth/Height bind
         the REAL box back into placement, so a tall tooltip never gets pinned
         by stale assumed metrics. -->
    <!-- aria-hidden: the canvas is role=presentation and the pointer-only
         hover card has no focusable content; announcing it would reference a
         node AT cannot reach. Commit details remain the accessible path. -->
    <div
      bind:clientWidth={tooltipBoxWidth}
      bind:clientHeight={tooltipBoxHeight}
      aria-hidden="true"
      class="pointer-events-none absolute left-0 top-0 z-30 w-80 max-w-[calc(100%_-_1rem)]"
      style="transform: translate3d({tooltipLeft}px, {tooltipTop}px, 0);"
    >
      <GraphNodeTooltip
        row={tooltipRow}
        refs={refsByCommit.get(tooltipRow.id) ?? []}
        placement={tooltipPlacement}
        caretX={tooltipAnchorX}
      />
    </div>
  {/if}
</div>
