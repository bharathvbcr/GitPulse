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
    type GraphHitKind,
    type GraphTheme,
    type VisualCommitRow,
  } from "../canvas/GraphRenderer";
  import { authorIdentity } from "../authors/authorIdentity";
  import { formatRelativeTime } from "../format";
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
    clampGraphScrollLeft,
    graphContentBoxStyle,
    graphViewportBoxStyle,
    resolveGraphLayout,
    resolveGraphOverflow,
  } from "../graph/graphLayout";
  import {
    applyGraphGutterWheel,
    canvasPointFromClient,
    graphDragScrollLeft,
    isGraphPanGesture,
    panGraphHorizontally,
    positionGraphTooltip,
    type TooltipPlacement,
  } from "../canvas/graphInteraction";
  import { portal } from "../dom/portal";
  import { LAYERS } from "../ui/layers";
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
  let graphViewport: HTMLDivElement | null = $state(null);
  let canvas: HTMLCanvasElement | null = $state(null);
  let scrollTop = $state(0);
  let containerHeight = $state(600);
  let rootWidth = $state(1_200);

  const renderer = new GraphRenderer(DENSITY_CONFIGS[get(densityStore)]);
  const scheduler = createFrameScheduler();

  let rowHeight = $derived(DENSITY_CONFIGS[$densityStore].rowHeight);

  let gpuCtx: CanvasRenderingContext2D | null = null;
  let attachedCanvas: HTMLCanvasElement | null = null;
  let hoveredCommitId: string | null = null;
  let tooltipRow: (typeof filteredRows)[number] | null = $state(null);
  /** What the pointer landed on; shapes the tooltip's context strip. */
  let tooltipHitKind: GraphHitKind = $state("node");
  /** Connector hits: the merge point the hovered descent lands on. */
  let tooltipMergeTarget: VisualCommitRow | null = $state(null);
  /**
   * Who owns the open tooltip. Pointer cards are decorative duplicates of
   * on-screen content and stay aria-hidden; a keyboard-focus card IS the
   * user's UI, is exposed to assistive tech, and survives pointer misses
   * until the row blurs or Escape dismisses it.
   */
  let tooltipSource: "pointer" | "focus" = $state("pointer");
  /** Live-region text announcing the focused commit's graph context. */
  let focusAnnouncement = $state("");
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

  // Single owner of gutter math (GraphRenderer.measureWidth): the highest
  // column occupied by a node, pass-through, or live connector. Lanes are
  // stable columns (the solver's interval allocation keeps width equal to
  // peak concurrent occupancy), so the gutter genuinely reaches every
  // branch — transient holes included — and never shifts under a hover.
  let laneAreaWidth = $derived(renderer.measureWidth(filteredRows));
  let graphLayout = $derived(
    resolveGraphLayout({
      measuredLaneWidth: laneAreaWidth,
      avatarSlotWidth: avatarSlot.width,
      availableWidth: rootWidth,
      widthMode: $interfaceStore.graphWidthMode,
    }),
  );
  let graphContentWidth = $derived(graphLayout.contentWidth);
  let graphViewportWidth = $derived(graphLayout.viewportWidth);
  let graphScrollLeft = $state(0);
  let graphOverflow = $derived(
    resolveGraphOverflow(graphScrollLeft, graphViewportWidth, graphContentWidth),
  );
  let gutterPan: {
    x: number;
    y: number;
    scrollLeft: number;
    moved: boolean;
  } | null = null;
  let suppressGraphClick = false;
  let graphPanning = $state(false);
  /** Centre X of the avatar column; null when the option is off. */
  let avatarCenterX = $derived(
    showAvatars && avatarSlot.width > 0
      ? graphContentWidth - renderer.getConfig().originX - avatarSlot.radius
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
  /**
   * Commits per author identity across the loaded (filtered) history, for
   * the avatar-hover tooltip. Keyed by the canonical identity key (email
   * first, then name) so display-name changes do not split one person's
   * count. Memoized on `filteredRows` identity by `$derived`.
   */
  let authorCommitCounts = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const row of filteredRows) {
      const key = authorIdentity(row.author_name, row.author_email).key;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return counts;
  });

  /**
   * Merge destination per closing commit: the parent a commit's
   * first-parent edge lands on when it leaves its own column. This is the
   * relationship the graph draws as a descending connector; rows carry it
   * as screen-reader text and both tooltip modes name it, so the
   * information is never pointer-only.
   */
  let closeTargetById = $derived.by(() => {
    const map = new Map<string, VisualCommitRow>();
    for (let i = 0; i < filteredRows.length; i++) {
      const first = filteredRows[i].connections?.[0];
      if (!first || first.is_dangling || first.is_merge) continue;
      if (!Number.isFinite(first.to_lane) || first.to_lane === first.from_lane) continue;
      const target = i + first.to_row_offset;
      if (!Number.isFinite(target) || target <= i || target >= filteredRows.length) continue;
      map.set(filteredRows[i].id, filteredRows[target]);
    }
    return map;
  });

  function authorCountFor(row: VisualCommitRow): number | null {
    return (
      authorCommitCounts.get(authorIdentity(row.author_name, row.author_email).key) ?? null
    );
  }

  /** One spoken sentence carrying what the tooltip card shows. */
  function composeFocusAnnouncement(row: VisualCommitRow): string {
    const kind = row.is_merge ? "Merge commit" : row.is_root ? "Root commit" : "Commit";
    const parts = [
      `${kind} ${row.id.slice(0, 7)}: ${row.summary || "no commit message"}.`,
      `By ${row.author_name || "unknown"}, ${formatRelativeTime(row.timestamp) || "unknown time"}.`,
    ];
    const target = closeTargetById.get(row.id);
    if (target) {
      parts.push(`Merges into ${target.id.slice(0, 7)}: ${target.summary || "no commit message"}.`);
    }
    const count = authorCountFor(row);
    if (count !== null && count > 0) {
      parts.push(`${count} ${count === 1 ? "commit" : "commits"} by this author in the loaded history.`);
    }
    return parts.join(" ");
  }

  /**
   * Keyboard focus on a commit row shows the same tooltip card the pointer
   * gets — anchored beside the row — and announces its content. Click
   * focus (`keyboardVisible` false) only announces; the pointer already
   * has its own affordance and a second card would double the UI.
   */
  function handleRowFocus(
    row: VisualCommitRow,
    element: HTMLElement,
    keyboardVisible: boolean,
  ) {
    focusAnnouncement = composeFocusAnnouncement(row);
    if (!keyboardVisible || !root) return;
    tooltipSource = "focus";
    tooltipRow = row;
    tooltipHitKind = "node";
    tooltipMergeTarget = closeTargetById.get(row.id) ?? null;
    hoveredCommitId = row.id;
    schedulePaint();
    const rect = element.getBoundingClientRect();
    tooltipPointer = { x: rect.left + 24, y: rect.top + rect.height / 2 };
    placeTooltip(tooltipPointer.x, tooltipPointer.y);
  }

  function handleRowBlur() {
    focusAnnouncement = "";
    if (tooltipSource !== "focus") return;
    tooltipSource = "pointer";
    tooltipRow = null;
    tooltipMergeTarget = null;
    tooltipPointer = null;
    hoveredCommitId = null;
    schedulePaint();
  }

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
    // Viewport box, not the canvas: the canvas is the full content width and
    // its left edge moves with overflow pan. Caching that desynced hit-testing
    // from painted branch nodes after a horizontal scroll. The viewport box is
    // stable; scrollLeft is added at hit time.
    if (!graphViewport) return null;
    if (!gutterRect) gutterRect = graphViewport.getBoundingClientRect();
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
      widthCss: graphContentWidth,
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
      tooltipMergeTarget = null;
      hoveredCommitId = null;
    }
    schedulePaint();
  }

  function handleGraphWheel(e: WheelEvent) {
    if (!container || !graphViewport) return;
    if (applyGraphGutterWheel(e, graphViewport, container, rowHeight)) {
      e.preventDefault();
    }
  }

  function hitAt(clientX: number, clientY: number) {
    const rect = canvasRect();
    if (!rect || !graphViewport) return null;
    const { x, y } = canvasPointFromClient(clientX, clientY, {
      left: rect.left,
      top: rect.top,
      scrollLeft: graphViewport.scrollLeft,
    });
    return renderer.getGraphHitAtPoint(
      x,
      y,
      filteredRows,
      startIndex,
      endIndex,
      scrollTop,
      showAvatars ? avatarCenterX : null,
    );
  }

  function handleGraphScroll() {
    // Re-hit under the parked pointer so a newly revealed branch node gets
    // its tooltip instead of keeping a miss from the previous content offset.
    gutterRect = null;
    if (graphViewport) graphScrollLeft = graphViewport.scrollLeft;
    if (lastPointer && !gutterPan?.moved) {
      pendingPointer = lastPointer;
      schedulePaint();
    } else if (!gutterPan?.moved) {
      tooltipRow = null;
      tooltipMergeTarget = null;
      hoveredCommitId = null;
    }
  }

  function handleGraphPointerDown(e: PointerEvent) {
    if (e.button !== 0 || !graphViewport) return;
    if (graphViewport.scrollWidth <= graphViewport.clientWidth + 0.5) return;
    suppressGraphClick = false;
    gutterPan = {
      x: e.clientX,
      y: e.clientY,
      scrollLeft: graphViewport.scrollLeft,
      moved: false,
    };
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }

  function handleGraphPointerUp() {
    if (gutterPan?.moved) suppressGraphClick = true;
    gutterPan = null;
    graphPanning = false;
  }

  function handleCanvasClick(e: MouseEvent) {
    if (suppressGraphClick) {
      suppressGraphClick = false;
      return;
    }
    // Connector hits attribute to the commit that owns the descent, so
    // clicking anywhere along a branch's closing line selects that commit.
    const hit = hitAt(e.clientX, e.clientY);
    if (hit) {
      graphStore.selectCommit(hit.row);
      repoStore.selectCommitDiff(hit.row.id);
    }
  }

  /**
   * Pointer moves only record the latest position; the frame callback below
   * performs at most one hit test per vsync (latest-wins). A drag that
   * exceeds the pan threshold pans extra lanes instead of selecting.
   */
  function handleCanvasMouseMove(e: PointerEvent) {
    lastPointer = { x: e.clientX, y: e.clientY };
    if (gutterPan && graphViewport && (e.buttons & 1) !== 0) {
      if (
        !gutterPan.moved &&
        isGraphPanGesture(gutterPan.x, gutterPan.y, e.clientX, e.clientY)
      ) {
        gutterPan.moved = true;
        graphPanning = true;
        tooltipRow = null;
        tooltipMergeTarget = null;
        hoveredCommitId = null;
      }
      if (gutterPan.moved) {
        const next = graphDragScrollLeft(
          gutterPan.scrollLeft,
          gutterPan.x,
          e.clientX,
        );
        panGraphHorizontally(graphViewport, next - graphViewport.scrollLeft);
        return;
      }
    }
    pendingPointer = lastPointer;
    schedulePaint();
  }

  function processPointer(x: number, y: number) {
    const hit = hitAt(x, y);
    if (!hit || !root) {
      if (canvas) canvas.style.cursor = "default";
      // A keyboard-focus card outlives pointer misses: an idle mouse over
      // empty gutter must not dismiss the UI a keyboard user is reading.
      if (tooltipSource === "focus") return;
      hoveredCommitId = null;
      tooltipRow = null;
      tooltipMergeTarget = null;
      tooltipPointer = null;
      return;
    }
    if (canvas) canvas.style.cursor = "pointer";
    tooltipSource = "pointer";
    hoveredCommitId = hit.row.id;
    tooltipPointer = { x, y };
    tooltipRow = hit.row;
    tooltipHitKind = hit.kind;
    tooltipMergeTarget = hit.connectorTarget;
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
    tooltipLeft = position.left + rootRect.left;
    tooltipTop = position.top + rootRect.top;
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
    if (gutterPan?.moved) return;
    if (canvas) canvas.style.cursor = "default";
    pendingPointer = null;
    lastPointer = null;
    // Leaving the canvas ends pointer hovers only; a keyboard-focus card
    // belongs to the focused row and dismisses on blur or Escape instead.
    if (tooltipSource === "focus") return;
    if (hoveredCommitId !== null) {
      hoveredCommitId = null;
      schedulePaint();
    }
    tooltipRow = null;
    tooltipMergeTarget = null;
  }

  $effect(() => {
    graphViewportWidth;
    graphContentWidth;
    if (!graphViewport) return;
    const next = clampGraphScrollLeft(
      graphViewport.scrollLeft,
      graphViewportWidth,
      graphContentWidth,
    );
    if (graphViewport.scrollLeft !== next) graphViewport.scrollLeft = next;
    if (graphScrollLeft !== next) graphScrollLeft = next;
  });

  $effect(() => {
    filteredRows;
    startIndex;
    endIndex;
    containerHeight;
    graphContentWidth;
    graphViewportWidth;
    $repoStore.selectedCommitId;
    // The canvas ring must not wait on the async diff round-trip: selecting
    // updates graphStore synchronously, repoStore.selectedCommitId only after
    // the diff resolves (or never, when it fails). Both are dependencies.
    $graphStore.selectedCommit;
    $graphStore.headId;
    $densityStore;
    $interfaceStore.showGraphAvatars;
    $interfaceStore.graphWidthMode;
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
    //
    // Validity is judged BY COMMIT ID, never by object identity: `tooltipRow`
    // is $state, and Svelte 5 deep-proxies assigned objects, so the value
    // read back is never `===` the raw row inside `filteredRows`. The old
    // identity check silently destroyed every tooltip one frame after it
    // mounted — caught only by driving the real UI, because jsdom component
    // tests never run this effect loop.
    const tooltipStale = (row: VisualCommitRow | null) =>
      row !== null && !filteredRows.some((r) => r.id === row.id);
    if (tooltipStale(tooltipRow) || tooltipStale(tooltipMergeTarget)) {
      tooltipRow = null;
      tooltipMergeTarget = null;
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
      for (const entry of entries) {
        // Distinguish "no measurement" from a real 0px measurement: a
        // collapsed pane must not become a phantom viewport.
        if (entry.target === container) containerHeight = entry.contentRect.height || 0;
        if (entry.target === root) rootWidth = entry.contentRect.width || 0;
      }
      gutterRect = null;
      rootRect = null;
    });
    if (container) observer.observe(container);
    if (root) {
      rootWidth = root.clientWidth || 0;
      observer.observe(root);
    }

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
    class="relative z-10 flex min-w-0 shrink-0 flex-col self-stretch border-r border-border/40 bg-background"
    style={graphViewportBoxStyle(graphViewportWidth)}
  >
    <div
      bind:this={graphViewport}
      class="min-h-0 min-w-0 flex-1 overflow-x-auto overflow-y-hidden gp-graph-hscroll cursor-pointer {graphPanning ? 'cursor-grabbing' : ''}"
      onclick={handleCanvasClick}
      onpointerdown={handleGraphPointerDown}
      onpointermove={handleCanvasMouseMove}
      onpointerup={handleGraphPointerUp}
      onpointercancel={handleGraphPointerUp}
      onpointerleave={handleCanvasMouseLeave}
      onwheel={handleGraphWheel}
      onscroll={handleGraphScroll}
      role="presentation"
    >
      <div style={graphContentBoxStyle(graphContentWidth)}>
        <canvas bind:this={canvas} class="gp-gpu w-full h-full block"></canvas>
      </div>
    </div>
    {#if graphOverflow.showStartFade}
      <div class="pointer-events-none absolute inset-y-0 left-0 z-20 w-8 bg-gradient-to-r from-background to-transparent"></div>
    {/if}
    {#if graphOverflow.showEndFade}
      <div class="pointer-events-none absolute inset-y-0 right-0 z-20 w-8 bg-gradient-to-l from-background to-transparent"></div>
    {/if}
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
            mergeTarget={closeTargetById.get(row.id) ?? null}
            onFocusRow={(element, keyboardVisible) =>
              handleRowFocus(row, element, keyboardVisible)}
            onBlurRow={handleRowBlur}
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
    <!-- Portaled to body: main.gp-pane uses contain:paint, which clips
         position:absolute descendants and traps them under the composited
         graph scroller. Fixed + body matches BranchList / ViewTabBar menus.
         Translate3d avoids layout invalidation at pointer frequency. -->
    <!-- Pointer cards are aria-hidden: the canvas is role=presentation and
         a mouse hover duplicates content sighted users see; announcing it
         would reference a node AT cannot reach. A KEYBOARD-focus card is
         different — it is that user's UI, so it stays exposed and its
         content is additionally spoken through the live region below. -->
    <div
      use:portal={"body"}
      bind:clientWidth={tooltipBoxWidth}
      bind:clientHeight={tooltipBoxHeight}
      aria-hidden={tooltipSource === "focus" ? undefined : true}
      class="pointer-events-none fixed left-0 top-0 w-80 max-w-[calc(100vw_-_1rem)]"
      style="transform: translate3d({tooltipLeft}px, {tooltipTop}px, 0); z-index: {LAYERS.TOOLTIP};"
    >
      <GraphNodeTooltip
        row={tooltipRow}
        refs={refsByCommit.get(tooltipRow.id) ?? []}
        placement={tooltipPlacement}
        caretX={tooltipAnchorX}
        hitKind={tooltipHitKind}
        mergeTarget={tooltipMergeTarget ?? closeTargetById.get(tooltipRow.id) ?? null}
        authorCommitCount={authorCountFor(tooltipRow)}
      />
    </div>
  {/if}

  <!-- Spoken mirror of the focus card: focusing a commit row announces the
       same graph context the pointer tooltip shows, merge destination and
       author stats included, so none of it is pointer-only. -->
  <div class="sr-only" role="status" aria-live="polite">{focusAnnouncement}</div>
</div>
