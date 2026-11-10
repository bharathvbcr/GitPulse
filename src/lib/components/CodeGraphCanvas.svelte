<script module lang="ts">
  /**
   * Globe or flat survives the canvas unmounting (a switch to a non-graph tab)
   * for the rest of the session. It is not persisted anywhere.
   */
  let globeChoice = false;
</script>

<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { acquireGpu2dContext, syncCanvasBackingStore } from "../canvas/gpuContext";
  import {
    DEFAULT_CODE_GRAPH_THEME,
    getCodeGraphColor,
    hitTestCodeGraph,
    paintCodeGraph,
    type CodeGraphGlobeFrame,
    type CodeGraphTheme,
  } from "../canvas/codeGraph/CodeGraphRenderer";
  import { codeGraphThemeFromElement } from "../canvas/codeGraph/themeFromCss";
  import {
    buildCodeGraphModel,
    CODE_GRAPH_LAYOUT_HEIGHT,
    CODE_GRAPH_LAYOUT_WIDTH,
    graphLanguageLegend,
    truncationLegend,
    type CodeGraphModel,
  } from "../codeintel/graphPayload";
  import {
    clampPitch,
    layoutOnSphere,
    lerpRotation,
    projectOrthographic,
    rotateByDrag,
    rotationToFace,
    type GlobeView,
    type ProjectedPoint,
  } from "../codeintel/globeLayout";
  import { buildGraphIndex, filterGraphNodes, fitGraphView, graphNodeOpenPath, traceGraph, type TraceDirection } from "../codeintel/graphNavigation";
  import type { GraphVizLoad } from "../codeintel/types";
  import { openSuspectsFor } from "../codeintel/openSuspects";
  import { createFrameScheduler } from "../motion/frameScheduler";
  import { easeOutCubic, motionDuration, prefersReducedMotion } from "../motion/easing";
  import { observeResize } from "../dom/observeResize";
  import { themeStore } from "../stores/themeStore";
  import { getLanguageIconColor, type LanguageIconKey } from "../language/languageLogos";
  import { popover } from "../ui/popover";
  import { LAYERS } from "../ui/layers";
  import Skeleton from "./Skeleton.svelte";

  /**
   * Code / subsystem graph canvas for the Map section.
   *
   * Draws `viz::build_payload` or `map_preview::build_preview_payload` JSON.
   * The legend always surfaces `counts.nodes_truncated` honesty.
   */

  interface Props {
    load: GraphVizLoad | null;
    loading?: boolean;
    /** Stable repository + view identity; refresh generations must not change it. */
    scopeKey?: string;
    /** Called when the user activates a node (double-click / Enter). */
    onOpenNode?: (path: string, nodeId: string) => void;
    /**
     * Select this node and bring it to the middle of the view (flat or globe),
     * clearing any filter that hides it. Applied once per id, as soon as the
     * node is in the model; set it to null (or another id) to re-arm.
     */
    focusNodeId?: string | null;
  }

  let { load = null, loading = false, scopeKey = "", onOpenNode, focusNodeId = null }: Props = $props();

  const FOCUS_TWEEN_MS = 280;
  const KEY_ROTATION = (10 * Math.PI) / 180;
  const GLOBE_ZOOM_MIN = 0.4, GLOBE_ZOOM_MAX = 4;

  let canvasEl: HTMLCanvasElement | null = $state(null);
  let hostEl: HTMLDivElement | null = $state(null);
  let width = $state(640);
  let height = $state(420);
  let panX = $state(0);
  let panY = $state(0);
  let scale = $state(1);
  let globeOn = $state(globeChoice);
  let yaw = $state(0);
  let pitch = $state(0);
  let globeZoom = $state(1);
  let selectedId = $state<string | null>(null);
  let hoveredId = $state<string | null>(null);
  let query = $state("");
  let activeCommunity = $state<string | null>(null);
  let showLabels = $state(true);
  let selectedLanguage = $state<LanguageIconKey | null>(null);
  let hideTests = $state(false), hideGenerated = $state(false), connectedOnly = $state(false);
  let hideNotes = $state(false);
  let showNodeList = $state(false);
  let filtersOpen = $state(false);
  let page = $state(0), connectionPage = $state(0);
  let communityPage = $state(0);
  let direction = $state<TraceDirection>("both");
  let depth = $state(1);
  let previousScope: string | null = null;
  let previousModel: CodeGraphModel | null = null;
  // A scope change fits an empty model when the payload has not landed yet
  // (fitGraphView of nothing is the identity). Plain, not $state: only the
  // effect below reads it, and it must not re-run that effect.
  let needsFit = false;
  let appliedFocusId: string | null = null;
  let dragging = false;
  let dragDistance = 0;
  let pointerStart = { x: 0, y: 0 };
  let lastPointer = { x: 0, y: 0 };
  let tweenFrame = 0;
  let themeFor: boolean | null = null;
  let theme: CodeGraphTheme = DEFAULT_CODE_GRAPH_THEME;
  const projectionScratch: ProjectedPoint[] = [];

  const scheduler = createFrameScheduler();
  // Hover hit testing walks every node and link; one pass per frame, from the
  // newest pointer position, instead of one per pointer event.
  const hoverScheduler = createFrameScheduler();
  let gpuCtx: CanvasRenderingContext2D | null = null;
  let stopResize: (() => void) | null = null;

  const payload = $derived(
    load?.available ? load.payload ?? null : null,
  );
  // Laid out once per payload in a fixed content space. The viewport is only
  // a pan/zoom window onto it, so a splitter drag repaints but never re-lays
  // out (up to 100k nodes and millions of neighbour visits per frame).
  const model = $derived.by((): CodeGraphModel =>
    buildCodeGraphModel(payload, CODE_GRAPH_LAYOUT_WIDTH, CODE_GRAPH_LAYOUT_HEIGHT),
  );
  const legend = $derived(
    truncationLegend(
      model.counts,
      payload?.meta?.coverage,
    ),
  );
  const index = $derived(buildGraphIndex(model));
  const selectedNode = $derived(selectedId ? index.nodes.get(selectedId) ?? null : null);

  // The globe is for the file and symbol graphs. Subsystems carry no level and
  // the doc graph supplies its own coordinates, so both stay flat.
  const globeAvailable = $derived(/^(?:file|symbol)s?$/.test(model.level ?? ""));
  const globeActive = $derived(globeOn && globeAvailable);
  // Laid out on the sphere only while the globe is showing.
  const unitPoints = $derived.by(() => globeActive ? layoutOnSphere(model) : []);
  const nodeOrder = $derived(new Map(model.nodes.map((node, i) => [node.id, i])));

  const matches = $derived(filterGraphNodes(model, index, {query, community:activeCommunity, language:selectedLanguage, hideTests, hideGenerated, hideNotes, connectedOnly}));
  const visibleIds = $derived(new Set(matches.map(n => n.id)));
  const filtersActive = $derived(Boolean(query || activeCommunity || selectedLanguage || hideTests || hideGenerated || hideNotes || connectedOnly));
  const inspectedNode = $derived(selectedNode ?? (hoveredId ? index.nodes.get(hoveredId) ?? null : null));
  const openPath = $derived(inspectedNode ? graphNodeOpenPath(inspectedNode) : null);
  const trace = $derived(selectedId || hoveredId ? traceGraph(index, selectedId || hoveredId || "", direction, depth, visibleIds) : null);
  const pageCount = $derived(Math.max(1, Math.ceil(matches.length / 30)));
  const communityPages = $derived(Math.max(1, Math.ceil(model.communities.length / 40)));
  const currentCommunityPage = $derived(Math.min(communityPage, communityPages - 1));
  const currentPage = $derived(Math.min(page, pageCount - 1));
  const connections = $derived(selectedNode ? [...new Set([...(index.outgoing.get(selectedNode.id) ?? []), ...(index.incoming.get(selectedNode.id) ?? []), ...(index.related.get(selectedNode.id) ?? [])])] : []);
  const connectionRows = $derived.by(() => {
    if (!selectedNode) return [];
    return (["outgoing", "incoming", "related"] as const).flatMap(side => (index[side].get(selectedNode.id) ?? []).flatMap(edgeIndex => {
      const link = model.links[edgeIndex];
      const node = index.nodes.get(side === "related" ? (link.source === selectedNode.id ? link.target : link.source) : side === "outgoing" ? link.target : link.source);
      return node ? [{side, edgeIndex, node, link}] : [];
    }));
  });
  const visibleConnections = $derived(connectionRows.filter(row => visibleIds.has(row.node.id) && (row.side === "related" || direction === "both" || row.side === direction)));
  const connectionPages = $derived(Math.max(1, Math.ceil(visibleConnections.length / 20)));
  const currentConnectionPage = $derived(Math.min(connectionPage, connectionPages - 1));
  const light = $derived($themeStore === "light");
  const languages = $derived(graphLanguageLegend(model.nodes));
  const zoomValue = $derived(globeActive ? globeZoom : scale);
  const zoomMin = $derived(globeActive ? GLOBE_ZOOM_MIN : 0.000001);
  const zoomMax = $derived(globeActive ? GLOBE_ZOOM_MAX : 5);
  const canvasLabel = $derived(globeActive
    ? "Code map, globe view. Drag to rotate, scroll to zoom. Arrow keys rotate; plus and minus zoom; Enter opens selection; Escape clears."
    : "Code map. Drag to pan, scroll to zoom. Arrow keys pan; plus and minus zoom; Enter opens selection; Escape clears.");

  /** Where a node reads: the path context that its display label may leave out. */
  function nodePathText(node: { path?: string; id: string }): string {
    return node.path || node.id;
  }

  function schedulePaint() {
    scheduler.schedule(() => {
      paint();
    });
  }

  function globeRadius(): number {
    return Math.max(40, Math.min(width, height) / 2 - 28);
  }

  function globeView(): GlobeView {
    return { yaw, pitch, zoom: globeZoom, cx: width / 2, cy: height / 2, radius: globeRadius() };
  }

  /** The unit layout projected for the current rotation, into one reused array. */
  function globeFrame(): CodeGraphGlobeFrame | null {
    if (!globeActive) return null;
    const view = globeView();
    return { projection: projectOrthographic(unitPoints, view, projectionScratch), cx: view.cx, cy: view.cy, radius: view.radius * view.zoom };
  }

  /** Token colours are read where the DOM is, once per theme flip. */
  function currentTheme(): CodeGraphTheme {
    if (hostEl && themeFor !== light) {
      theme = codeGraphThemeFromElement(hostEl);
      themeFor = light;
    }
    return theme;
  }

  function paint() {
    if (!canvasEl) return;
    if (!gpuCtx) gpuCtx = acquireGpu2dContext(canvasEl, false);
    if (!gpuCtx) return;
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    syncCanvasBackingStore(canvasEl, gpuCtx, width, height, dpr);
    // The frame scheduler coalesces interaction. Paint the current model
    // directly: counts/generation are not a valid cache key for graph content.
    paintCodeGraph(gpuCtx, {
      model, widthCss: width, heightCss: height, panX, panY, scale,
      selectedId, hoveredId, visibleIds, traceIds:trace?.ids, traceEdges:trace?.edges, showLabels, light,
      theme: currentTheme(), activeCommunity, globe: globeFrame(),
    });
  }

  function measure() {
    if (!hostEl) return;
    const rect = hostEl.getBoundingClientRect();
    width = Math.max(160, Math.floor(rect.width));
    height = Math.max(160, Math.floor(rect.height));
    schedulePaint();
  }

  function cancelTween() {
    if (tweenFrame && typeof cancelAnimationFrame === "function") cancelAnimationFrame(tweenFrame);
    tweenFrame = 0;
  }

  function onPointerDown(e: PointerEvent) {
    if (!canvasEl || e.button !== 0) return;
    canvasEl.focus();
    canvasEl.setPointerCapture(e.pointerId);
    hoverScheduler.cancel();
    cancelTween();
    dragging = true;
    lastPointer = { x: e.clientX, y: e.clientY };
    pointerStart = lastPointer;
    dragDistance = 0;
  }

  function onPointerMove(e: PointerEvent) {
    if (!canvasEl) return;
    // Drag arithmetic is in client space (deltas), so it never reads layout.
    const pt = { x: e.clientX, y: e.clientY };
    if (dragging) {
      dragDistance = Math.max(dragDistance, Math.hypot(pt.x - pointerStart.x, pt.y - pointerStart.y));
      if (globeActive) {
        const turned = rotateByDrag({ yaw, pitch }, pt.x - lastPointer.x, pt.y - lastPointer.y, globeRadius() * globeZoom);
        yaw = turned.yaw;
        pitch = turned.pitch;
      } else {
        panX += pt.x - lastPointer.x;
        panY += pt.y - lastPointer.y;
      }
      lastPointer = pt;
      schedulePaint();
      return;
    }
    hoverScheduler.schedule(() => {
      if (!canvasEl || dragging) return;
      const rect = canvasEl.getBoundingClientRect();
      const hit = hitTestCodeGraph(model, pt.x - rect.left, pt.y - rect.top, panX, panY, scale, visibleIds, globeFrame());
      const next = hit?.kind === "node" ? hit.id : null;
      if (next !== hoveredId) {
        hoveredId = next;
        schedulePaint();
      }
    });
  }

  function onPointerUp(e: PointerEvent) {
    dragging = false;
    if (!canvasEl) return;
    try {
      canvasEl.releasePointerCapture(e.pointerId);
    } catch {
      // Pointer cancellation can release capture before this handler runs.
    }
  }

  function onClick(e: MouseEvent) {
    if (!canvasEl || dragDistance > 4) return;
    const rect = canvasEl.getBoundingClientRect();
    const hit = hitTestCodeGraph(
      model,
      e.clientX - rect.left,
      e.clientY - rect.top,
      panX,
      panY,
      scale,
      visibleIds,
      globeFrame(),
    );
    selectedId = hit?.kind === "node" ? hit.id : null;
    schedulePaint();
  }

  function onDblClick(e: MouseEvent) {
    if (!canvasEl || !onOpenNode) return;
    const rect = canvasEl.getBoundingClientRect();
    const hit = hitTestCodeGraph(
      model,
      e.clientX - rect.left,
      e.clientY - rect.top,
      panX,
      panY,
      scale,
      visibleIds,
      globeFrame(),
    );
    if (hit?.kind === "node" && hit.node) {
      const path = graphNodeOpenPath(hit.node);
      if (path) onOpenNode(path, hit.node.id);
    }
  }

  function zoomAt(factor: number, x = width / 2, y = height / 2) {
    if (globeActive) {
      globeZoom = Math.min(GLOBE_ZOOM_MAX, Math.max(GLOBE_ZOOM_MIN, globeZoom * factor));
      schedulePaint();
      return;
    }
    const next = Math.min(5, Math.max(0.000001, scale * factor));
    panX = x - (x - panX) * next / scale;
    panY = y - (y - panY) * next / scale;
    scale = next;
    schedulePaint();
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (!canvasEl || e.deltaY === 0) return;
    const rect = canvasEl.getBoundingClientRect();
    zoomAt(Math.exp(-Math.max(-120, Math.min(120, e.deltaY)) * 0.002), e.clientX - rect.left, e.clientY - rect.top);
  }

  function resetFlatView() {
    const fitted = fitGraphView(matches, width, height);
    panX = fitted.panX;
    panY = fitted.panY;
    scale = fitted.scale;
  }

  function resetGlobeView() {
    yaw = 0;
    pitch = 0;
    globeZoom = 1;
  }

  function resetView() {
    cancelTween();
    if (globeActive) resetGlobeView(); else resetFlatView();
    schedulePaint();
  }

  /** Turn the globe so a node faces the viewer; a jump under reduced motion. */
  function faceNode(id: string) {
    const at = nodeOrder.get(id);
    const point = at === undefined ? undefined : unitPoints[at];
    if (!point) return;
    cancelTween();
    const target = rotationToFace(point);
    const duration = motionDuration(FOCUS_TWEEN_MS);
    if (prefersReducedMotion() || duration === 0 || typeof requestAnimationFrame !== "function") {
      yaw = target.yaw;
      pitch = target.pitch;
      schedulePaint();
      return;
    }
    const from = { yaw, pitch };
    const began = performance.now();
    const step = (now: number) => {
      const t = Math.min(1, (now - began) / duration);
      const turned = lerpRotation(from, target, easeOutCubic(t));
      yaw = turned.yaw;
      pitch = turned.pitch;
      schedulePaint();
      tweenFrame = t < 1 ? requestAnimationFrame(step) : 0;
    };
    tweenFrame = requestAnimationFrame(step);
  }

  function focusNode(id: string) {
    const node = index.nodes.get(id);
    if (!node || !visibleIds.has(id)) return;
    selectedId = id;
    query = "";
    showNodeList = false;
    connectionPage = 0;
    if (globeActive) {
      faceNode(id);
      return;
    }
    scale = Math.max(1.5, scale);
    panX = (width > 760 ? (width - 340) / 2 : width / 2) - node.x * scale;
    panY = height * (width > 760 ? 0.5 : 0.65) - node.y * scale;
    schedulePaint();
  }

  function setGlobe(on: boolean) {
    cancelTween();
    globeOn = on;
    globeChoice = on;
    hoveredId = null;
    if (on && globeAvailable && selectedId) faceNode(selectedId);
    schedulePaint();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") { clearFilters(); showNodeList = false; }
    else if (e.key === "+" || e.key === "=") zoomAt(1.2);
    else if (e.key === "-") zoomAt(1 / 1.2);
    else if (e.key === "0") resetView();
    else if (e.key === "Enter" && selectedNode && onOpenNode) {
      const path = graphNodeOpenPath(selectedNode);
      if (path) onOpenNode(path, selectedNode.id);
    }
    else if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) {
      if (globeActive) {
        cancelTween();
        yaw += e.key === "ArrowLeft" ? KEY_ROTATION : e.key === "ArrowRight" ? -KEY_ROTATION : 0;
        pitch = clampPitch(pitch + (e.key === "ArrowUp" ? KEY_ROTATION : e.key === "ArrowDown" ? -KEY_ROTATION : 0));
      } else {
        panX += e.key === "ArrowLeft" ? 40 : e.key === "ArrowRight" ? -40 : 0;
        panY += e.key === "ArrowUp" ? 40 : e.key === "ArrowDown" ? -40 : 0;
      }
    } else return;
    e.preventDefault();
    schedulePaint();
  }

  // A lost 2D context leaves every later paint drawing into a dead one, so the
  // map freezes. Drop the cached context and repaint once it is back (the
  // browser only fires contextrestored when the context is usable again).
  function onContextLost(event: Event) {
    event.preventDefault();
    gpuCtx = null;
    schedulePaint();
  }

  function onContextRestored() {
    gpuCtx = null;
    schedulePaint();
  }

  onMount(() => {
    measure();
    // Updating responsive classes inside an observer delivery can resize the
    // observed stage again. Defer and coalesce writes outside that delivery.
    if (hostEl) stopResize = observeResize(hostEl, () => measure());
    const canvas = canvasEl;
    canvas?.addEventListener("contextlost", onContextLost);
    canvas?.addEventListener("contextrestored", onContextRestored);
    return () => {
      canvas?.removeEventListener("contextlost", onContextLost);
      canvas?.removeEventListener("contextrestored", onContextRestored);
    };  });

  onDestroy(() => {
    scheduler.cancel();
    hoverScheduler.cancel();
    cancelTween();
    stopResize?.();
    stopResize = null;
    gpuCtx = null;
  });

  function filtersChanged() {
    selectedId = null; hoveredId = null; page = 0; connectionPage = 0;
  }

  function clearFilters() {
    query = ""; activeCommunity = null; selectedLanguage = null;
    hideTests = false; hideGenerated = false; hideNotes = false; connectedOnly = false;
    filtersChanged();
  }

  const filtersDismissal = {
    dismiss: { inside: "[data-map-filters]", escape: "bubble" as const },
    onDismiss: () => { filtersOpen = false; },
  };

  $effect(() => {
    const nextModel = model;
    const nextScope = scopeKey || load?.path || "default";
    const available = load?.available;
    untrack(() => {
      cancelTween();
      if (nextScope !== previousScope) {
        clearFilters(); resetFlatView(); resetGlobeView(); schedulePaint(); showNodeList = false; direction = "both"; depth = 1;
        filtersOpen = false;
        communityPage = 0;
        previousModel = null;
        needsFit = nextModel.nodes.length === 0;
      } else if (available) {
        // First payload for this scope: fit it once. Later same-scope refreshes
        // keep the user's zoom and pan.
        if (needsFit && nextModel.nodes.length > 0) {
          resetFlatView(); resetGlobeView(); schedulePaint();
          needsFit = false;
        }
        const oldNode = previousModel?.nodes.find(n => n.id === selectedId);
        const newNode = nextModel.nodes.find(n => n.id === selectedId);
        // The flat view re-anchors on the selection; the globe is positioned
        // by rotation, which a refresh leaves alone.
        if (oldNode && newNode && !globeActive) {
          panX += (oldNode.x - newNode.x) * scale;
          panY += (oldNode.y - newNode.y) * scale;
        }
        if (!newNode || !visibleIds.has(newNode.id)) selectedId = null;
        if (hoveredId && !visibleIds.has(hoveredId)) hoveredId = null;
        if (!nextModel.communities.some(c => c.name === activeCommunity)) activeCommunity = null;
      }
      previousScope = nextScope;
      if (available) previousModel = nextModel;
    });
  });

  // After the scope effect on purpose: a scope change fits the view, and the
  // requested node is brought forward over that fit, not the other way round.
  $effect(() => {
    const wanted = focusNodeId;
    const present = wanted ? index.nodes.has(wanted) : false;
    untrack(() => {
      if (!wanted) { appliedFocusId = null; return; }
      if (!present || appliedFocusId === wanted) return;
      appliedFocusId = wanted;
      if (!visibleIds.has(wanted)) clearFilters();
      focusNode(wanted);
    });
  });

  $effect(() => {
    // Re-layout when payload or size changes.
    void model;
    void width;
    void height;
    void visibleIds;
    void trace;
    void activeCommunity;
    void selectedId;
    void hoveredId;
    void showLabels;
    void light;
    void globeActive;
    void yaw;
    void pitch;
    void globeZoom;
    schedulePaint();
  });
</script>

<div class="code-map bg-background flex flex-col min-h-0 flex-1" class:narrow={width <= 760}>
  <div class="map-toolbar flex flex-wrap items-center gap-x-3 gap-y-2 px-5 pt-4 pb-3">
    <div class="map-heading min-w-0 mr-auto">
      <span class="block text-xs font-medium text-textMuted">Dependency map</span>
      {#if loading}
        <Skeleton variant="text" width="12rem" height="0.75rem" class="mt-1.5" />
      {:else}
        <div class="map-counts" role="status" data-testid="code-graph-legend">
          {#if !load?.available}
            <span>{load?.reason ?? "Graph unavailable"}</span>
          {:else}
            <span data-testid="code-graph-nodes-label">{legend.nodesLabel}</span>
            {#if legend.linksLabel}<span>· {legend.linksLabel}</span>{/if}
            <span>· {model.communities.length} groups</span>
            <span>·</span><span data-testid="graph-filter-count">{matches.length} of {model.nodes.length} match filters</span>
            {#if filtersActive}<button type="button" class="gp-btn px-2 py-0.5" data-testid="map-clear-filters" onclick={clearFilters}>Clear filters</button>{/if}
          {/if}
        </div>
      {/if}
    </div>
    <div class="map-actions flex flex-wrap items-center gap-2 min-w-0">
      <label class="map-search gp-field flex items-center gap-2 text-textMuted">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="10" cy="10" r="6"/><path d="m15 15 5 5"/></svg>
        <input class="w-44 min-w-0 bg-transparent outline-hidden text-textPrimary" aria-label="Find a file or symbol" placeholder="Find a file or symbol…" bind:value={query} oninput={filtersChanged}
          onkeydown={(e) => { if (e.key === "Enter" && matches[0]) focusNode(matches[0].id); }} />
        {#if query}<button type="button" class="gp-icon-btn -mr-1.5 p-0.5" aria-label="Clear search" onclick={() => query = ""}>×</button>{/if}
      </label>
      <button type="button" class="gp-btn" data-testid="map-nodes-toggle" class:text-accent={showNodeList} aria-pressed={showNodeList} onclick={() => showNodeList = !showNodeList}>Nodes</button>
      <button type="button" class="gp-btn" class:text-accent={showLabels} aria-pressed={showLabels} onclick={() => showLabels = !showLabels}>Labels</button>
      {#if globeAvailable}
        <div class="gp-segmented" role="group" aria-label="Layout" data-testid="map-layout">
          <button type="button" class="gp-seg-btn" data-active={!globeActive} aria-pressed={!globeActive} onclick={() => setGlobe(false)}>Flat</button>
          <button type="button" class="gp-seg-btn" data-active={globeActive} aria-pressed={globeActive} onclick={() => setGlobe(true)}>Globe</button>
        </div>
      {/if}
      <div class="relative" data-map-filters>
        <button type="button" class="gp-btn" data-testid="map-filters-toggle" class:text-accent={filtersActive} aria-haspopup="true" aria-expanded={filtersOpen} onclick={() => filtersOpen = !filtersOpen}>Filters</button>
        {#if filtersOpen}
          <div class="map-filters gp-menu absolute right-0 top-full mt-1 w-64 max-h-[70vh] overflow-y-auto! text-xs" style={`z-index:${LAYERS.MENU}`}
            use:popover={filtersDismissal} role="group" aria-label="Graph filters">
            <label class="gp-menu-item cursor-pointer" title="Identified by conventional test directory and filename patterns"><input class="accent-accent" type="checkbox" bind:checked={hideTests} onchange={filtersChanged}/> Hide tests</label>
            <label class="gp-menu-item cursor-pointer" title="Identified by conventional generated, build, and vendor paths"><input class="accent-accent" type="checkbox" bind:checked={hideGenerated} onchange={filtersChanged}/> Hide generated/vendor</label>
            <label class="gp-menu-item cursor-pointer" title="Keep nodes with relationships in the loaded graph"><input class="accent-accent" type="checkbox" bind:checked={connectedOnly} onchange={filtersChanged}/> Connected only</label>
            <label class="gp-menu-item cursor-pointer" title="Hide document and note nodes, or subsystems made entirely of documentation"><input class="accent-accent" type="checkbox" aria-label="Hide notes & Markdown" bind:checked={hideNotes} onchange={filtersChanged}/> Hide notes &amp; Markdown</label>
            {#if languages.length > 0}
              <div class="gp-menu-sep" role="separator"></div>
              <div class="map-languages" role="group" aria-label="Node colors by language">
                <button type="button" class="gp-menu-item" data-map-language="" aria-pressed={!selectedLanguage} class:text-accent={!selectedLanguage} onclick={() => { selectedLanguage = null; filtersChanged(); }}>All languages</button>
                {#each languages as language (language.key)}
                  <button type="button" class="gp-menu-item" data-map-language={language.key} title={`${language.count} nodes in loaded graph`} aria-pressed={selectedLanguage === language.key} class:text-accent={selectedLanguage === language.key}
                    onclick={() => { selectedLanguage = selectedLanguage === language.key ? null : language.key; filtersChanged(); }}>
                    <span class="community-dot" style={`background:${getLanguageIconColor(language.key, $themeStore)}`}></span>
                    <span class="min-w-0 flex-1 truncate">{language.name}</span><span class="chip-count">{language.count}</span>
                  </button>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      </div>
    </div>
  </div>
  {#if legend.honesty || legend.coverageLabel}
    <p class="map-honesty" data-testid="code-graph-truncation" role="note">
      {legend.honesty ?? ""} {legend.coverageLabel ?? ""}
    </p>
  {/if}
  {#if model.warnings.length}
    <p class="map-honesty" role="note">{model.warnings.join(" ")}</p>
  {/if}
  {#if model.communities.length > 0}
    <div class="map-group-row">
    <div class="map-communities" aria-label="Communities">
      <button type="button" class="gp-chip cursor-pointer hover:bg-surfaceHover {activeCommunity ? 'text-textMuted' : 'bg-surfaceHover text-textPrimary'}" data-map-group="" aria-pressed={!activeCommunity} onclick={() => { activeCommunity = null; filtersChanged(); }}>All groups</button>
      {#each model.communities.slice(currentCommunityPage * 40, (currentCommunityPage + 1) * 40) as community (community.name)}
        <button type="button" class="gp-chip cursor-pointer hover:bg-surfaceHover {activeCommunity === community.name ? 'bg-surfaceHover text-textPrimary' : 'text-textMuted'}" data-map-group={community.name}
          aria-pressed={activeCommunity === community.name}
          title={`${community.name} · ${community.shown} shown of ${community.count}`}
          onclick={() => { activeCommunity = activeCommunity === community.name ? null : community.name; filtersChanged(); }}>
          <span class="min-w-0 truncate">{community.label}</span><span class="chip-count">{community.shown}</span>
        </button>
      {/each}
    </div>
    {#if communityPages > 1}<div class="group-pages">
      <button type="button" class="gp-icon-btn" aria-label="Previous groups" disabled={currentCommunityPage === 0} onclick={() => communityPage = currentCommunityPage - 1}>‹</button>
      <span title="Group pages">{currentCommunityPage + 1}/{communityPages}</span>
      <button type="button" class="gp-icon-btn" aria-label="Next groups" disabled={currentCommunityPage + 1 >= communityPages} onclick={() => communityPage = currentCommunityPage + 1}>›</button>
    </div>{/if}
    </div>
  {/if}

  <div class="map-stage flex-1 min-h-0 relative" bind:this={hostEl}>
    <canvas bind:this={canvasEl}
      class="absolute inset-0 w-full h-full cursor-grab active:cursor-grabbing"
      style={`width:${width}px;height:${height}px;touch-action:none`}
      onpointerdown={onPointerDown} onpointermove={onPointerMove} onpointerup={onPointerUp}
      onpointercancel={onPointerUp} onpointerleave={() => { if (!dragging) hoveredId = null; }}
      onclick={onClick} ondblclick={onDblClick} onwheel={onWheel} onkeydown={onKeyDown}
      tabindex="0" aria-label={canvasLabel}
    ></canvas>

    {#if query.trim() || showNodeList}
      <div class="map-results" role="region" aria-label={query.trim() ? "Search results" : "Node browser"}>
        <div class="result-heading">{matches.length} {query.trim() ? "matches" : "nodes"} · {matches.length ? currentPage * 30 + 1 : 0}–{Math.min(matches.length, (currentPage + 1) * 30)} shown</div>
        {#each matches.slice(currentPage * 30, (currentPage + 1) * 30) as node (node.id)}
          <button type="button" onclick={() => focusNode(node.id)} class="gp-menu-item" class:bg-surfaceHover={selectedId === node.id} title={nodePathText(node)}>
            <span class="community-dot" style={`background:${getCodeGraphColor(node, light)}`}></span>
            <span class="result-copy"><strong>{node.label}</strong><small>{nodePathText(node)}</small></span>
          </button>
        {/each}
        {#if matches.length === 0}<p>No nodes match the current filters.</p>{/if}
        {#if pageCount > 1}<div class="map-pagination">
          <button type="button" class="gp-btn px-2 py-0.5" aria-label="Previous nodes" disabled={currentPage === 0} onclick={() => page = currentPage - 1}>Previous</button>
          <span>{currentPage + 1} / {pageCount}</span>
          <button type="button" class="gp-btn px-2 py-0.5" aria-label="Next nodes" disabled={currentPage + 1 >= pageCount} onclick={() => page = currentPage + 1}>Next</button>
        </div>{/if}
      </div>
    {:else if selectedNode}
      <aside class="map-neighbors" aria-label="Node connections">
        <div class="neighbor-heading"><strong>Connections</strong><button type="button" class="gp-icon-btn" aria-label="Close connections" onclick={() => selectedId = null}>×</button></div>
        {#if load?.kind === "code_graph"}
          <!-- A code-graph node id is a DevMap node id (a file, or file::symbol),
               which is exactly what the suspects walk resolves a symptom by.
               Subsystem and doc nodes are not, so they do not offer it. -->
          <button type="button" class="gp-btn node-suspects" onclick={() => openSuspectsFor(selectedNode.id)} title="Which commit since the last known good could have broken {selectedNode.name}, ranked by what the call graph reaches from it">Find suspects for {selectedNode.name}</button>
        {/if}
        <div class="trace-controls">
          <select class="gp-select min-w-0 flex-1 text-xs" aria-label="Trace direction" bind:value={direction} onchange={() => connectionPage = 0}>
            <option value="both">Both directions</option><option value="outgoing">Depends on</option><option value="incoming">Used by</option>
          </select>
          <select class="gp-select min-w-0 flex-1 text-xs" aria-label="Trace depth" bind:value={depth}><option value={1}>1 hop</option><option value={2}>2 hops</option><option value={3}>3 hops</option></select>
        </div>
        <p class="trace-summary" role="status">{trace?.ids.size ?? 0} nodes in trace · {visibleConnections.length} of {connectionRows.length} direct relationships match filters/direction</p>
        {#if trace?.truncated}<p class="map-honesty" role="note">Trace limit reached (1,000 nodes or 50,000 edge visits). More nodes may be reachable.</p>{/if}
        <div class="neighbor-list">
          {#each visibleConnections.slice(currentConnectionPage * 20, (currentConnectionPage + 1) * 20) as row (`${row.side}:${row.edgeIndex}`)}
            <button type="button" class="gp-menu-item" onclick={() => focusNode(row.node.id)} title={nodePathText(row.node)}>
              <span class="community-dot" style={`background:${getCodeGraphColor(row.node, light)}`}></span>
              <span class="result-copy"><strong>{row.node.label}</strong><small>{nodePathText(row.node)}</small><small>{row.side === "related" ? "Related to" : row.side === "outgoing" ? "Depends on" : "Used by"} · {row.link.kind || "relationship"}</small>
                <small>{row.link.evidence_count ?? 1} source relationship(s) · {row.link.confidence == null ? "confidence unknown" : `${Math.round(row.link.confidence * 100)}% confidence`}</small>
              </span>
            </button>
          {/each}
          {#if visibleConnections.length === 0}<p>No direct relationships match this view.</p>{/if}
        </div>
        {#if connectionPages > 1}<div class="map-pagination">
          <button type="button" class="gp-btn px-2 py-0.5" aria-label="Previous connections" disabled={currentConnectionPage === 0} onclick={() => connectionPage = currentConnectionPage - 1}>Previous</button>
          <span>{currentConnectionPage + 1} / {connectionPages}</span>
          <button type="button" class="gp-btn px-2 py-0.5" aria-label="Next connections" disabled={currentConnectionPage + 1 >= connectionPages} onclick={() => connectionPage = currentConnectionPage + 1}>Next</button>
        </div>{/if}
      </aside>
    {/if}

    {#if !loading && load?.available && model.nodes.length === 0}
      <div class="map-empty"><strong>No nodes to display</strong><span>This graph payload is empty.</span></div>
    {:else if !loading && load?.available && matches.length === 0}
      <div class="map-empty"><strong>No nodes match the current filters</strong><span>Clear filters to show all loaded nodes.</span></div>
    {/if}

    <div class="map-zoom" aria-label="Graph view controls">
      <button type="button" class="gp-icon-btn h-7 w-7" aria-label="Zoom out" onclick={() => zoomAt(1 / 1.2)} disabled={zoomValue <= zoomMin}>−</button>
      <span>{zoomValue < 0.01 ? "<1" : Math.round(zoomValue * 100)}%</span>
      <button type="button" class="gp-icon-btn h-7 w-7" aria-label="Zoom in" onclick={() => zoomAt(1.2)} disabled={zoomValue >= zoomMax}>+</button>
      <button type="button" class="gp-btn ml-1" data-testid="map-fit" onclick={resetView} title={globeActive ? "Face the globe front-on (0)" : "Fit filtered nodes into the view (0)"}>{globeActive ? "Reset view" : "Fit view"}</button>
    </div>
    <div class="map-help">{globeActive ? "Drag to rotate" : "Drag to explore"} <span>·</span> Scroll to zoom <span>·</span> Select to trace connections</div>
  </div>

  <div class="map-inspector bg-surface/50" data-testid="code-graph-selection">
    {#if inspectedNode}
      <span class="community-dot" style={`background:${getCodeGraphColor(inspectedNode, light)}`}></span>
      <div class="inspector-copy">
        <strong title={nodePathText(inspectedNode)}>{inspectedNode.label}</strong>
        <span title={nodePathText(inspectedNode)}>{nodePathText(inspectedNode)}</span>
      </div>
      <span class="inspector-kind">{inspectedNode.kind ?? model.level ?? "node"}</span>
      {#if selectedNode}<span class="connection-count">{connections.length} loaded connections</span>{/if}
      {#if inspectedNode.flags?.length}<span class="inspector-kind">{inspectedNode.flags.map(f => f.flag).join(" · ")}</span>{/if}
      {#if onOpenNode && openPath}
        <button type="button" class="gp-btn" onclick={() => { if (inspectedNode && openPath) onOpenNode?.(openPath, inspectedNode.id); }}>Open file ↗</button>
      {/if}
      {#if selectedNode}<button type="button" class="gp-icon-btn" aria-label="Clear selection" onclick={() => selectedId = null}>×</button>{/if}
    {:else}
      <span class="inspector-placeholder">Select a node to inspect its file and connections.</span>
      <span class="inspector-key">Node size reflects connectivity</span>
    {/if}
  </div>
</div>

<style>
  .code-map { min-width: 0; color: rgb(var(--c-text));
    --map-panel: var(--mac-fill-surface, rgb(var(--c-surface)));
    --map-border: rgb(var(--c-border) / 0.45);
    --map-muted: rgb(var(--c-text-muted));
    --map-hover: rgb(var(--c-surface-hover) / 0.55); }
  .map-counts { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin-top: 5px; font-size: 11px; color: var(--map-muted); }
  canvas:focus-visible { outline: 2px solid rgb(var(--c-accent)); outline-offset: 2px; }
  .map-honesty { margin: 0; padding: 0 20px 10px; color: rgb(var(--c-status-amber)); font-size: 11px; }
  .map-group-row { display: flex; min-width: 0; }
  .group-pages { display: flex; flex: none; align-items: center; gap: 4px; padding: 0 12px 12px 0; font-size: 10px; color: var(--map-muted); }
  .map-communities { display: flex; flex: 1; min-width: 0; gap: 5px; overflow-x: auto; padding: 0 20px 12px; scrollbar-width: thin; }
  .map-communities > :global(.gp-chip) { flex: none; max-width: 220px; }
  .community-dot { width: 6px; height: 6px; border-radius: 50%; flex: none; display: inline-block; }
  .chip-count { opacity: .55; font-variant-numeric: tabular-nums; }
  .map-neighbors { position: absolute; right: 16px; top: 8px; width: 304px; max-height: calc(100% - 76px); overflow: auto; padding: 10px; border: 1px solid var(--map-border); border-radius: 10px; background: var(--map-panel); box-shadow: var(--shadow-pop); }
  .neighbor-heading, .trace-controls, .map-pagination { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .neighbor-heading { font-size: 11px; margin-bottom: 9px; }
  .node-suspects { width: 100%; margin-bottom: 9px; justify-content: flex-start; }
  .trace-summary, .neighbor-list p { margin: 8px 0; font-size: 10px; line-height: 1.5; color: var(--map-muted); }
  .map-pagination { padding: 5px; font-size: 10px; color: var(--map-muted); }
  .map-stage { overflow: hidden; background-image: radial-gradient(circle, rgb(var(--c-text-muted) / 0.08) .65px, transparent .65px); background-size: 22px 22px; }
  .map-zoom { position: absolute; right: 16px; bottom: 16px; display: flex; align-items: center; padding: 4px; border: 1px solid var(--map-border); border-radius: 9px; background: var(--map-panel); box-shadow: var(--shadow-pop); }
  .map-zoom span { width: 42px; text-align: center; font-size: 10px; color: var(--map-muted); font-variant-numeric: tabular-nums; }
  .map-help { position: absolute; bottom: 25px; left: 20px; font-size: 10px; color: var(--map-muted); pointer-events: none; }
  .map-help span { padding: 0 6px; opacity: .4; }
  .map-results { position: absolute; top: 8px; left: 16px; width: min(310px, calc(100% - 32px)); max-height: 60%; overflow-y: auto; padding: 6px; border-radius: 10px; border: 1px solid var(--map-border); background: var(--map-panel); box-shadow: var(--shadow-pop); }
  .result-heading, .map-results p { padding: 7px; font-size: 10px; color: var(--map-muted); }
  .result-copy { display: flex; flex-direction: column; gap: 3px; overflow: hidden; }
  .result-copy strong { font-size: 11px; font-weight: 500; }
  .result-copy strong, .inspector-copy strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .result-copy small { font-size: 10px; color: var(--map-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .map-empty { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; font-size: 12px; color: var(--map-muted); pointer-events: none; }
  .map-inspector { min-height: 55px; padding: 10px 20px; display: flex; align-items: center; gap: 12px; font-size: 11px; }
  .inspector-copy { display: flex; flex: 1; flex-direction: column; gap: 3px; min-width: 0; }
  .inspector-copy strong { font-weight: 500; }
  .inspector-copy span { color: var(--map-muted); font-size: 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .inspector-kind, .connection-count { color: var(--map-muted); font-size: 10px; max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .inspector-placeholder { flex: 1; color: var(--map-muted); }
  .inspector-key { color: var(--map-muted); font-size: 10px; opacity: .7; }
  .narrow .map-neighbors { left: 16px; width: auto; max-height: 38%; }
  .narrow .map-inspector { flex-wrap: wrap; }
  .narrow .map-help, .narrow .inspector-key { display: none; }
  .narrow .connection-count { max-width: 90px; }
</style>
