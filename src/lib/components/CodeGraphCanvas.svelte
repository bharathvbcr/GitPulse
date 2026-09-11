<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { acquireGpu2dContext, syncCanvasBackingStore } from "../canvas/gpuContext";
  import {
    DEFAULT_CODE_GRAPH_THEME,
    getCodeGraphColor,
    hitTestCodeGraph,
    paintCodeGraph,
  } from "../canvas/codeGraph/CodeGraphRenderer";
  import {
    buildCodeGraphModel,
    graphLanguageLegend,
    truncationLegend,
    type CodeGraphModel,
  } from "../codeintel/graphPayload";
  import { buildGraphIndex, filterGraphNodes, fitGraphView, graphNodeOpenPath, traceGraph, type TraceDirection } from "../codeintel/graphNavigation";
  import type { GraphVizLoad } from "../codeintel/types";
  import { createFrameScheduler } from "../motion/frameScheduler";
  import { observeResize } from "../dom/observeResize";
  import { themeStore } from "../stores/themeStore";
  import { getLanguageIconColor, type LanguageIconKey } from "../language/languageLogos";

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
  }

  let { load = null, loading = false, scopeKey = "", onOpenNode }: Props = $props();

  let canvasEl: HTMLCanvasElement | null = $state(null);
  let hostEl: HTMLDivElement | null = $state(null);
  let width = $state(640);
  let height = $state(420);
  let panX = $state(0);
  let panY = $state(0);
  let scale = $state(1);
  let selectedId = $state<string | null>(null);
  let hoveredId = $state<string | null>(null);
  let query = $state("");
  let activeCommunity = $state<string | null>(null);
  let showLabels = $state(true);
  let selectedLanguage = $state<LanguageIconKey | null>(null);
  let hideTests = $state(false), hideGenerated = $state(false), connectedOnly = $state(false);
  let hideNotes = $state(false);
  let showNodeList = $state(false);
  let page = $state(0), connectionPage = $state(0);
  let communityPage = $state(0);
  let direction = $state<TraceDirection>("both");
  let depth = $state(1);
  let previousScope: string | null = null;
  let previousModel: CodeGraphModel | null = null;
  let dragging = false;
  let dragDistance = 0;
  let pointerStart = { x: 0, y: 0 };
  let lastPointer = { x: 0, y: 0 };

  const scheduler = createFrameScheduler();
  let gpuCtx: CanvasRenderingContext2D | null = null;
  let stopResize: (() => void) | null = null;

  const payload = $derived(
    load?.available ? load.payload ?? null : null,
  );
  const model = $derived.by((): CodeGraphModel =>
    buildCodeGraphModel(payload, width, height),
  );
  const legend = $derived(
    truncationLegend(
      model.counts,
      payload?.meta?.coverage,
    ),
  );
  const index = $derived(buildGraphIndex(model));
  const selectedNode = $derived(selectedId ? index.nodes.get(selectedId) ?? null : null);

  const matches = $derived(filterGraphNodes(model, index, {query, community:activeCommunity, language:selectedLanguage, hideTests, hideGenerated, hideNotes, connectedOnly}));
  const visibleIds = $derived(new Set(matches.map(n => n.id)));
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

  function schedulePaint() {
    scheduler.schedule(() => {
      paint();
    });
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
      theme: {
        ...DEFAULT_CODE_GRAPH_THEME,
        label: light ? "#374151" : "#c8d2e1",
        linkMuted: light ? "#a8b5c6" : "#435067",
        selection: light ? "#172b4d" : "#ecf3ff",
      },
    });
  }

  function measure() {
    if (!hostEl) return;
    const rect = hostEl.getBoundingClientRect();
    width = Math.max(160, Math.floor(rect.width));
    height = Math.max(160, Math.floor(rect.height));
    schedulePaint();
  }

  function pointerToLocal(e: PointerEvent): { x: number; y: number } {
    const rect = canvasEl!.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  }

  function onPointerDown(e: PointerEvent) {
    if (!canvasEl || e.button !== 0) return;
    canvasEl.focus();
    canvasEl.setPointerCapture(e.pointerId);
    dragging = true;
    lastPointer = pointerToLocal(e);
    pointerStart = lastPointer;
    dragDistance = 0;
  }

  function onPointerMove(e: PointerEvent) {
    if (!canvasEl) return;
    const pt = pointerToLocal(e);
    if (dragging) {
      dragDistance = Math.max(dragDistance, Math.hypot(pt.x - pointerStart.x, pt.y - pointerStart.y));
      panX += pt.x - lastPointer.x;
      panY += pt.y - lastPointer.y;
      lastPointer = pt;
      schedulePaint();
      return;
    }
    const hit = hitTestCodeGraph(model, pt.x, pt.y, panX, panY, scale, visibleIds);
    const next = hit?.kind === "node" ? hit.id : null;
    if (next !== hoveredId) {
      hoveredId = next;
      schedulePaint();
    }
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
    );
    if (hit?.kind === "node" && hit.node) {
      const path = graphNodeOpenPath(hit.node);
      if (path) onOpenNode(path, hit.node.id);
    }
  }

  function zoomAt(factor: number, x = width / 2, y = height / 2) {
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

  function resetView() {
    const fitted = fitGraphView(matches, width, height);
    panX = fitted.panX;
    panY = fitted.panY;
    scale = fitted.scale;
    schedulePaint();
  }

  function focusNode(id: string) {
    const node = index.nodes.get(id);
    if (!node || !visibleIds.has(id)) return;
    selectedId = id;
    query = "";
    showNodeList = false;
    connectionPage = 0;
    scale = Math.max(1.5, scale);
    panX = (width > 760 ? (width - 340) / 2 : width / 2) - node.x * scale;
    panY = height * (width > 760 ? 0.5 : 0.65) - node.y * scale;
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
      panX += e.key === "ArrowLeft" ? 40 : e.key === "ArrowRight" ? -40 : 0;
      panY += e.key === "ArrowUp" ? 40 : e.key === "ArrowDown" ? -40 : 0;
    } else return;
    e.preventDefault();
    schedulePaint();
  }

  onMount(() => {
    measure();
    // Updating responsive classes inside an observer delivery can resize the
    // observed stage again. Defer and coalesce writes outside that delivery.
    if (hostEl) stopResize = observeResize(hostEl, () => measure());
  });

  onDestroy(() => {
    scheduler.cancel();
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

  $effect(() => {
    const nextModel = model;
    const nextScope = scopeKey || load?.path || "default";
    const available = load?.available;
    untrack(() => {
      if (nextScope !== previousScope) {
        clearFilters(); resetView(); showNodeList = false; direction = "both"; depth = 1;
        communityPage = 0;
        previousModel = null;
      } else if (available) {
        const oldNode = previousModel?.nodes.find(n => n.id === selectedId);
        const newNode = nextModel.nodes.find(n => n.id === selectedId);
        if (oldNode && newNode) {
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
    schedulePaint();
  });
</script>

<div class="code-map bg-background flex flex-col min-h-0 flex-1" class:narrow={width <= 760}>
  <div class="map-toolbar">
    <div class="map-heading">
      <span class="map-eyebrow">DEPENDENCY MAP</span>
      <div class="map-counts" role="status" data-testid="code-graph-legend">
        {#if loading}
          <span>Loading graph…</span>
        {:else if !load?.available}
          <span>{load?.reason ?? "Graph unavailable"}</span>
        {:else}
          <span data-testid="code-graph-nodes-label">{legend.nodesLabel}</span>
          {#if legend.linksLabel}<span>· {legend.linksLabel}</span>{/if}
          <span>· {model.communities.length} groups</span>
        {/if}
      </div>
    </div>
    <div class="map-actions">
      <label class="map-search">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><circle cx="10" cy="10" r="6"/><path d="m15 15 5 5"/></svg>
        <input aria-label="Find a file or symbol" placeholder="Find a file or symbol…" bind:value={query} oninput={filtersChanged}
          onkeydown={(e) => { if (e.key === "Enter" && matches[0]) focusNode(matches[0].id); }} />
        {#if query}<button type="button" aria-label="Clear search" onclick={() => query = ""}>×</button>{/if}
      </label>
      <button type="button" class="map-button" aria-pressed={showNodeList} onclick={() => showNodeList = !showNodeList}>Nodes</button>
      <button type="button" class="map-button" aria-pressed={showLabels} onclick={() => showLabels = !showLabels}>Labels</button>
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
      <button type="button" class="community-chip" class:chosen={!activeCommunity} aria-pressed={!activeCommunity} onclick={() => { activeCommunity = null; filtersChanged(); }}>All groups</button>
      {#each model.communities.slice(currentCommunityPage * 40, (currentCommunityPage + 1) * 40) as community (community.name)}
        <button type="button" class="community-chip" class:chosen={activeCommunity === community.name}
          aria-pressed={activeCommunity === community.name}
          title={`${community.name} · ${community.shown} shown of ${community.count}`}
          onclick={() => { activeCommunity = activeCommunity === community.name ? null : community.name; filtersChanged(); }}>
          <span>{community.label}</span><span class="chip-count">{community.shown}</span>
        </button>
      {/each}
    </div>
    {#if communityPages > 1}<div class="group-pages">
      <button type="button" aria-label="Previous groups" disabled={currentCommunityPage === 0} onclick={() => communityPage = currentCommunityPage - 1}>‹</button>
      <span title="Group pages">{currentCommunityPage + 1}/{communityPages}</span>
      <button type="button" aria-label="Next groups" disabled={currentCommunityPage + 1 >= communityPages} onclick={() => communityPage = currentCommunityPage + 1}>›</button>
    </div>{/if}
    </div>
  {/if}
  {#if languages.length > 0}
    <div class="map-languages" aria-label="Node colors by language">
      <button type="button" class="language-heading" aria-pressed={!selectedLanguage} onclick={() => { selectedLanguage = null; filtersChanged(); }}>All languages</button>
      {#each languages as language (language.key)}
        <button type="button" class="language-swatch" title={`${language.count} nodes in loaded graph`} aria-pressed={selectedLanguage === language.key}
          onclick={() => { selectedLanguage = selectedLanguage === language.key ? null : language.key; filtersChanged(); }}>
          <span class="community-dot" style={`background:${getLanguageIconColor(language.key, $themeStore)}`}></span>
          <span>{language.name}</span><span class="chip-count">{language.count}</span>
        </button>
      {/each}
    </div>
  {/if}

  <div class="map-filters" aria-label="Graph filters">
    <label title="Identified by conventional test directory and filename patterns"><input type="checkbox" bind:checked={hideTests} onchange={filtersChanged}/> Hide tests</label>
    <label title="Identified by conventional generated, build, and vendor paths"><input type="checkbox" bind:checked={hideGenerated} onchange={filtersChanged}/> Hide generated/vendor</label>
    <label title="Keep nodes with relationships in the loaded graph"><input type="checkbox" bind:checked={connectedOnly} onchange={filtersChanged}/> Connected only</label>
    <label title="Hide document and note nodes, or subsystems made entirely of documentation"><input type="checkbox" aria-label="Hide notes & Markdown" bind:checked={hideNotes} onchange={filtersChanged}/> Hide notes &amp; Markdown</label>
    <span role="status" data-testid="graph-filter-count">{matches.length} of {model.nodes.length} match filters</span>
    {#if query || activeCommunity || selectedLanguage || hideTests || hideGenerated || hideNotes || connectedOnly}<button type="button" onclick={clearFilters}>Clear filters</button>{/if}
  </div>

  <div class="map-stage flex-1 min-h-0 relative" bind:this={hostEl}>
    <canvas bind:this={canvasEl}
      class="absolute inset-0 w-full h-full cursor-grab active:cursor-grabbing"
      style={`width:${width}px;height:${height}px;touch-action:none`}
      onpointerdown={onPointerDown} onpointermove={onPointerMove} onpointerup={onPointerUp}
      onpointercancel={onPointerUp} onpointerleave={() => { if (!dragging) hoveredId = null; }}
      onclick={onClick} ondblclick={onDblClick} onwheel={onWheel} onkeydown={onKeyDown}
      tabindex="0" aria-label="Code graph canvas. Drag to pan, scroll to zoom. Arrow keys pan; plus and minus zoom; Enter opens selection; Escape clears."
    ></canvas>

    {#if query.trim() || showNodeList}
      <div class="map-results" role="region" aria-label={query.trim() ? "Search results" : "Node browser"}>
        <div class="result-heading">{matches.length} {query.trim() ? "matches" : "nodes"} · {matches.length ? currentPage * 30 + 1 : 0}–{Math.min(matches.length, (currentPage + 1) * 30)} shown</div>
        {#each matches.slice(currentPage * 30, (currentPage + 1) * 30) as node (node.id)}
          <button type="button" onclick={() => focusNode(node.id)} class:chosen={selectedId === node.id}>
            <span class="community-dot" style={`background:${getCodeGraphColor(node, light)}`}></span>
            <span class="result-copy"><strong>{node.name}</strong><small>{node.path || node.id}</small></span>
          </button>
        {/each}
        {#if matches.length === 0}<p>No nodes match the current filters.</p>{/if}
        {#if pageCount > 1}<div class="map-pagination">
          <button type="button" aria-label="Previous nodes" disabled={currentPage === 0} onclick={() => page = currentPage - 1}>Previous</button>
          <span>{currentPage + 1} / {pageCount}</span>
          <button type="button" aria-label="Next nodes" disabled={currentPage + 1 >= pageCount} onclick={() => page = currentPage + 1}>Next</button>
        </div>{/if}
      </div>
    {:else if selectedNode}
      <aside class="map-neighbors" aria-label="Node connections">
        <div class="neighbor-heading"><strong>Connections</strong><button type="button" aria-label="Close connections" onclick={() => selectedId = null}>×</button></div>
        <div class="trace-controls">
          <select aria-label="Trace direction" bind:value={direction} onchange={() => connectionPage = 0}>
            <option value="both">Both directions</option><option value="outgoing">Depends on</option><option value="incoming">Used by</option>
          </select>
          <select aria-label="Trace depth" bind:value={depth}><option value={1}>1 hop</option><option value={2}>2 hops</option><option value={3}>3 hops</option></select>
        </div>
        <p class="trace-summary" role="status">{trace?.ids.size ?? 0} nodes in trace · {visibleConnections.length} of {connectionRows.length} direct relationships match filters/direction</p>
        {#if trace?.truncated}<p class="map-honesty" role="note">Trace limit reached (1,000 nodes or 50,000 edge visits). More nodes may be reachable.</p>{/if}
        <div class="neighbor-list">
          {#each visibleConnections.slice(currentConnectionPage * 20, (currentConnectionPage + 1) * 20) as row (`${row.side}:${row.edgeIndex}`)}
            <button type="button" onclick={() => focusNode(row.node.id)}>
              <span class="community-dot" style={`background:${getCodeGraphColor(row.node, light)}`}></span>
              <span class="result-copy"><strong>{row.node.name}</strong><small>{row.side === "related" ? "Related to" : row.side === "outgoing" ? "Depends on" : "Used by"} · {row.link.kind || "relationship"}</small>
                <small>{row.link.evidence_count ?? 1} source relationship(s) · {row.link.confidence == null ? "confidence unknown" : `${Math.round(row.link.confidence * 100)}% confidence`}</small>
              </span>
            </button>
          {/each}
          {#if visibleConnections.length === 0}<p>No direct relationships match this view.</p>{/if}
        </div>
        {#if connectionPages > 1}<div class="map-pagination">
          <button type="button" aria-label="Previous connections" disabled={currentConnectionPage === 0} onclick={() => connectionPage = currentConnectionPage - 1}>Previous</button>
          <span>{currentConnectionPage + 1} / {connectionPages}</span>
          <button type="button" aria-label="Next connections" disabled={currentConnectionPage + 1 >= connectionPages} onclick={() => connectionPage = currentConnectionPage + 1}>Next</button>
        </div>{/if}
      </aside>
    {/if}

    {#if !loading && load?.available && model.nodes.length === 0}
      <div class="map-empty"><strong>No nodes to display</strong><span>This graph payload is empty.</span></div>
    {:else if !loading && load?.available && matches.length === 0}
      <div class="map-empty"><strong>No nodes match the current filters</strong><span>Clear filters to show all loaded nodes.</span></div>
    {/if}

    <div class="map-zoom" aria-label="Graph view controls">
      <button type="button" aria-label="Zoom out" onclick={() => zoomAt(1 / 1.2)} disabled={scale <= 0.000001}>−</button>
      <span>{scale < 0.01 ? "<1" : Math.round(scale * 100)}%</span>
      <button type="button" aria-label="Zoom in" onclick={() => zoomAt(1.2)} disabled={scale >= 5}>+</button>
      <button type="button" class="fit-button" onclick={resetView} title="Fit filtered nodes into the view (0)">Fit view</button>
    </div>
    <div class="map-help">Drag to explore <span>·</span> Scroll to zoom <span>·</span> Select to trace connections</div>
  </div>

  <div class="map-inspector" data-testid="code-graph-selection">
    {#if inspectedNode}
      <span class="community-dot" style={`background:${getCodeGraphColor(inspectedNode, light)}`}></span>
      <div class="inspector-copy">
        <strong title={inspectedNode.name}>{inspectedNode.name}</strong>
        <span title={inspectedNode.path || inspectedNode.id}>{inspectedNode.path || inspectedNode.id}</span>
      </div>
      <span class="inspector-kind">{inspectedNode.kind ?? model.level ?? "node"}</span>
      {#if selectedNode}<span class="connection-count">{connections.length} loaded connections</span>{/if}
      {#if inspectedNode.flags?.length}<span class="inspector-kind">{inspectedNode.flags.map(f => f.flag).join(" · ")}</span>{/if}
      {#if onOpenNode && openPath}
        <button type="button" class="map-button" onclick={() => { if (inspectedNode && openPath) onOpenNode?.(openPath, inspectedNode.id); }}>Open file ↗</button>
      {/if}
      {#if selectedNode}<button type="button" class="map-button" aria-label="Clear selection" onclick={() => selectedId = null}>×</button>{/if}
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
  .map-toolbar { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 12px; padding: 16px 20px 12px; }
  .map-eyebrow { font-size: 10px; font-weight: 650; letter-spacing: .13em; color: var(--map-muted); }
  .map-counts { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 5px; font-size: 11px; }
  .map-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; min-width: 0; }
  .map-search { display: flex; align-items: center; gap: 8px; background: var(--map-panel); border: 1px solid var(--map-border); border-radius: 8px; padding: 7px 10px; color: var(--map-muted); }
  .map-search input { width: 190px; min-width: 0; background: transparent; outline: none; font-size: 11px; color: inherit; }
  button { cursor: pointer; }
  button:disabled { opacity: .35; cursor: default; }
  button:focus-visible, canvas:focus-visible, .map-search:focus-within { outline: 2px solid #79a7ed; outline-offset: 2px; }
  .map-button { border: 1px solid var(--map-border); border-radius: 7px; padding: 6px 10px; font-size: 11px; white-space: nowrap; background: var(--map-panel); }
  .map-button:hover, .map-button[aria-pressed=true] { background: var(--map-hover); color: #79a7ed; }
  .map-honesty { margin: 0; padding: 0 20px 10px; color: light-dark(#936016, #c99756); font-size: 11px; }
  .map-group-row { display: flex; min-width: 0; }
  .group-pages { display: flex; flex: none; align-items: center; gap: 4px; padding: 0 12px 12px 0; font-size: 10px; color: var(--map-muted); }
  .group-pages button { padding: 5px; }
  .map-communities { display: flex; flex: 1; min-width: 0; gap: 5px; overflow-x: auto; padding: 0 20px 12px; scrollbar-width: thin; }
  .community-chip { display: inline-flex; align-items: center; gap: 6px; flex: none; padding: 5px 9px; border-radius: 6px; border: 1px solid transparent; color: var(--map-muted); font-size: 10px; }
  .community-chip:hover, .community-chip.chosen { background: var(--map-hover); border-color: var(--map-border); color: inherit; }
  .community-chip > span:first-child { max-width: 150px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .map-languages { display: flex; flex: none; gap: 14px; overflow-x: auto; padding: 0 20px 12px; font-size: 10px; color: var(--map-muted); scrollbar-width: thin; }
  .language-heading { font-weight: 600; white-space: nowrap; }
  .language-swatch[aria-pressed=true] { color: inherit; box-shadow: 0 1px currentColor; }
  .language-swatch { display: inline-flex; align-items: center; gap: 6px; flex: none; }
  .community-dot { width: 6px; height: 6px; border-radius: 50%; flex: none; display: inline-block; }
  .chip-count { opacity: .55; font-variant-numeric: tabular-nums; }
  .map-filters { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 14px; padding: 0 20px 10px; font-size: 10px; color: var(--map-muted); }
  .map-filters label { display: inline-flex; align-items: center; gap: 5px; }
  .map-filters input { accent-color: #79a7ed; }
  .map-filters button { color: inherit; text-decoration: underline; }
  .map-neighbors { position: absolute; right: 16px; top: 8px; width: 304px; max-height: calc(100% - 76px); overflow: auto; padding: 10px; border: 1px solid var(--map-border); border-radius: 10px; background: var(--map-panel); box-shadow: 0 8px 30px #00000025; }
  .neighbor-heading, .trace-controls, .map-pagination { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .neighbor-heading { font-size: 11px; margin-bottom: 9px; }
  .neighbor-heading button { width: 24px; height: 24px; }
  .trace-controls select { min-width: 0; padding: 5px; border-radius: 5px; background: var(--map-panel); color: inherit; border: 1px solid var(--map-border); font-size: 11px; }
  .trace-summary, .neighbor-list p { margin: 8px 0; font-size: 10px; line-height: 1.5; color: var(--map-muted); }
  .neighbor-list button { display: flex; align-items: center; gap: 8px; width: 100%; text-align: left; padding: 8px 4px; border-radius: 6px; }
  .neighbor-list button:hover { background: var(--map-hover); }
  .map-pagination { padding: 5px; font-size: 10px; color: var(--map-muted); }
  .map-results .map-pagination button, .map-pagination button { width: auto; padding: 5px; }
  .map-stage { overflow: hidden; background-image: radial-gradient(circle, #8b9db314 .65px, transparent .65px); background-size: 22px 22px; }
  .map-zoom { position: absolute; right: 16px; bottom: 16px; display: flex; align-items: center; padding: 4px; border: 1px solid var(--map-border); border-radius: 9px; background: var(--map-panel); box-shadow: 0 4px 18px #00000015; }
  .map-zoom button { width: 28px; height: 28px; border-radius: 5px; font-size: 17px; }
  .map-zoom button:hover { background: var(--map-hover); }
  .map-zoom span { width: 42px; text-align: center; font-size: 10px; color: var(--map-muted); font-variant-numeric: tabular-nums; }
  .map-zoom .fit-button { width: auto; padding: 0 10px; margin-left: 4px; font-size: 11px; }
  .map-help { position: absolute; bottom: 25px; left: 20px; font-size: 10px; color: var(--map-muted); pointer-events: none; }
  .map-help span { padding: 0 6px; opacity: .4; }
  .map-results { position: absolute; top: 8px; left: 16px; width: min(310px, calc(100% - 32px)); max-height: 60%; overflow-y: auto; padding: 6px; border-radius: 10px; border: 1px solid var(--map-border); background: var(--map-panel); box-shadow: 0 10px 40px #00000025; }
  .result-heading, .map-results p { padding: 7px; font-size: 10px; color: var(--map-muted); }
  .map-results button { display: flex; align-items: center; gap: 10px; width: 100%; text-align: left; padding: 8px; border-radius: 6px; }
  .map-results button:hover, .map-results button.chosen { background: var(--map-hover); }
  .result-copy { display: flex; flex-direction: column; gap: 3px; overflow: hidden; }
  .result-copy strong { font-size: 11px; font-weight: 500; }
  .result-copy strong, .inspector-copy strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .result-copy small { font-size: 10px; color: var(--map-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .map-empty { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 8px; font-size: 12px; color: var(--map-muted); pointer-events: none; }
  .map-inspector { min-height: 55px; padding: 10px 20px; display: flex; align-items: center; gap: 12px; border-top: 1px solid var(--map-border); font-size: 11px; }
  .inspector-copy { display: flex; flex: 1; flex-direction: column; gap: 3px; min-width: 0; }
  .inspector-copy strong { font-weight: 550; }
  .inspector-copy span { color: var(--map-muted); font-size: 10px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .inspector-kind, .connection-count { color: var(--map-muted); font-size: 10px; max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .inspector-placeholder { flex: 1; color: var(--map-muted); }
  .inspector-key { color: var(--map-muted); font-size: 10px; opacity: .7; }
  .narrow .map-neighbors { left: 16px; width: auto; max-height: 38%; }
  .narrow .map-inspector { flex-wrap: wrap; }
  .narrow .map-help, .narrow .inspector-key { display: none; }
  .narrow .connection-count { max-width: 90px; }
</style>
