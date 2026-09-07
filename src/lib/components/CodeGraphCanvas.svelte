<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { acquireGpu2dContext, syncCanvasBackingStore } from "../canvas/gpuContext";
  import { createOffscreenSurface, type CachedSurface } from "../canvas/graphCache";
  import {
    DEFAULT_CODE_GRAPH_THEME,
    hitTestCodeGraph,
    paintCodeGraph,
  } from "../canvas/codeGraph/CodeGraphRenderer";
  import {
    buildCodeGraphModel,
    truncationLegend,
    type CodeGraphModel,
  } from "../codeintel/graphPayload";
  import type { GraphVizLoad, GraphVizPayload } from "../codeintel/types";
  import { createFrameScheduler } from "../motion/frameScheduler";
  import { getBranchColor } from "../canvas/Palette";

  /**
   * Code / subsystem graph canvas for the Map section.
   *
   * Draws `viz::build_payload` or `map_preview::build_preview_payload` JSON.
   * The legend always surfaces `counts.nodes_truncated` honesty.
   */

  interface Props {
    load: GraphVizLoad | null;
    loading?: boolean;
    /** Called when the user activates a node (double-click / Enter). */
    onOpenNode?: (path: string, nodeId: string) => void;
  }

  let { load = null, loading = false, onOpenNode }: Props = $props();

  let canvasEl: HTMLCanvasElement | null = $state(null);
  let hostEl: HTMLDivElement | null = $state(null);
  let width = $state(640);
  let height = $state(420);
  let panX = $state(0);
  let panY = $state(0);
  let scale = $state(1);
  let selectedId = $state<string | null>(null);
  let hoveredId = $state<string | null>(null);
  let dragging = false;
  let lastPointer = { x: 0, y: 0 };

  const scheduler = createFrameScheduler();
  let gpuCtx: CanvasRenderingContext2D | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let staticLayer: CachedSurface | null = null;
  let staticKey = "";

  const payload = $derived(
    (load?.available ? load.payload : null) as GraphVizPayload | null,
  );
  const model = $derived.by((): CodeGraphModel =>
    buildCodeGraphModel(payload, Math.max(320, width), Math.max(240, height)),
  );
  const legend = $derived(
    truncationLegend(
      model.counts,
      payload?.meta && typeof payload.meta === "object"
        ? (payload.meta as { coverage?: { indexed_total?: number; in_subsystems?: number } })
            .coverage
        : null,
    ),
  );
  const selectedNode = $derived(
    selectedId ? model.nodes.find((n) => n.id === selectedId) ?? null : null,
  );

  function schedulePaint() {
    scheduler.schedule(() => {
      paint();
    });
  }

  function paint() {
    if (!canvasEl) return;
    if (!gpuCtx) {
      gpuCtx = acquireGpu2dContext(canvasEl, false);
    }
    const ctx = gpuCtx;
    if (!ctx) return;
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    syncCanvasBackingStore(canvasEl, ctx, width, height, dpr);

    // Offscreen surface from graphCache — same backing-store math as the commit
    // graph strips, without borrowing the lane renderer.
    const key = [
      model.nodes.length,
      model.links.length,
      model.generationId ?? "",
      width,
      height,
      panX.toFixed(1),
      panY.toFixed(1),
      scale.toFixed(3),
      selectedId ?? "",
      hoveredId ?? "",
      model.nodes.length <= 120 ? "labels" : "nolabels",
    ].join("|");

    if (staticKey !== key || !staticLayer) {
      staticLayer?.release?.();
      staticLayer = createOffscreenSurface(width, height, dpr, false);
      if (staticLayer) {
        staticLayer.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        paintCodeGraph(staticLayer.ctx, {
          model,
          widthCss: width,
          heightCss: height,
          panX,
          panY,
          scale,
          selectedId,
          hoveredId,
          theme: DEFAULT_CODE_GRAPH_THEME,
          showLabels: model.nodes.length <= 120,
        });
        staticKey = key;
      }
    }

    ctx.clearRect(0, 0, width, height);
    if (staticLayer) {
      ctx.drawImage(
        staticLayer.canvas,
        0,
        0,
        staticLayer.canvas.width,
        staticLayer.canvas.height,
        0,
        0,
        width,
        height,
      );
    } else {
      paintCodeGraph(ctx, {
        model,
        widthCss: width,
        heightCss: height,
        panX,
        panY,
        scale,
        selectedId,
        hoveredId,
        theme: DEFAULT_CODE_GRAPH_THEME,
        showLabels: model.nodes.length <= 120,
      });
    }
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
    if (!canvasEl) return;
    canvasEl.setPointerCapture(e.pointerId);
    dragging = true;
    lastPointer = pointerToLocal(e);
  }

  function onPointerMove(e: PointerEvent) {
    if (!canvasEl) return;
    const pt = pointerToLocal(e);
    if (dragging) {
      panX += pt.x - lastPointer.x;
      panY += pt.y - lastPointer.y;
      lastPointer = pt;
      schedulePaint();
      return;
    }
    const hit = hitTestCodeGraph(model, pt.x, pt.y, panX, panY, scale);
    const next = hit?.kind === "node" ? hit.id : hit?.kind === "link" ? hit.id : null;
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
      /* already released */
    }
  }

  function onClick(e: MouseEvent) {
    if (!canvasEl) return;
    const rect = canvasEl.getBoundingClientRect();
    const hit = hitTestCodeGraph(
      model,
      e.clientX - rect.left,
      e.clientY - rect.top,
      panX,
      panY,
      scale,
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
    );
    if (hit?.kind === "node" && hit.node) {
      const path = hit.node.path || hit.node.id;
      onOpenNode(path, hit.node.id);
    }
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const factor = e.deltaY > 0 ? 0.9 : 1.1;
    scale = Math.min(4, Math.max(0.25, scale * factor));
    schedulePaint();
  }

  function resetView() {
    panX = 0;
    panY = 0;
    scale = 1;
    schedulePaint();
  }

  onMount(() => {
    measure();
    resizeObserver = new ResizeObserver(() => measure());
    if (hostEl) resizeObserver.observe(hostEl);
  });

  onDestroy(() => {
    scheduler.cancel();
    resizeObserver?.disconnect();
    staticLayer?.release?.();
    staticLayer = null;
    gpuCtx = null;
  });

  $effect(() => {
    // Re-layout when payload or size changes.
    void model;
    void width;
    void height;
    schedulePaint();
  });
</script>

<div class="flex flex-col min-h-0 flex-1 gap-0">
  <!-- Legend / truncation honesty -->
  <div
    class="shrink-0 flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border/50 px-2.5 py-1.5 text-[11px] text-textMuted"
    role="status"
    data-testid="code-graph-legend"
  >
    {#if loading}
      <span>Loading graph…</span>
    {:else if !load?.available}
      <span class="text-amber-600 dark:text-amber-300">{load?.reason ?? "Graph unavailable"}</span>
    {:else}
      <span data-testid="code-graph-nodes-label" title="counts.nodes_shown / nodes_total">
        {legend.nodesLabel}
      </span>
      {#if legend.linksLabel}
        <span>· {legend.linksLabel}</span>
      {/if}
      {#if model.level}
        <span class="font-mono">· {model.level}</span>
      {/if}
      {#if model.generationId}
        <span>· gen {model.generationId}</span>
      {/if}
      {#if legend.coverageLabel}
        <span>· {legend.coverageLabel}</span>
      {/if}
      <button type="button" class="gp-btn text-[10px] px-1.5 py-0 ml-auto" onclick={resetView}>
        Reset view
      </button>
    {/if}
  </div>

  {#if legend.honesty}
    <p
      class="shrink-0 px-2.5 py-1 text-[10px] text-amber-600 dark:text-amber-300 border-b border-border/40"
      data-testid="code-graph-truncation"
      role="note"
    >
      {legend.honesty}
    </p>
  {/if}

  {#if model.communities.length > 0}
    <div
      class="shrink-0 flex flex-wrap gap-x-3 gap-y-1 px-2.5 py-1.5 text-[10px] text-textMuted border-b border-border/40 max-h-16 overflow-y-auto"
      aria-label="Communities"
    >
      {#each model.communities.slice(0, 12) as community, i (`${community.name}#${i}`)}
        <span class="inline-flex items-center gap-1">
          <span
            class="inline-block w-2 h-2 rounded-full"
            style={`background:${getBranchColor(community.colorIndex)}`}
          ></span>
          <span class="font-mono truncate max-w-[10rem]" title={community.name}>
            {community.name}
          </span>
          <span>({community.count})</span>
        </span>
      {/each}
      {#if model.communities.length > 12}
        <span>+{model.communities.length - 12} more</span>
      {/if}
    </div>
  {/if}

  <div class="flex-1 min-h-0 relative" bind:this={hostEl}>
    <canvas
      bind:this={canvasEl}
      class="absolute inset-0 w-full h-full cursor-grab active:cursor-grabbing"
      style={`width:${width}px;height:${height}px`}
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={onPointerUp}
      onpointercancel={onPointerUp}
      onclick={onClick}
      ondblclick={onDblClick}
      onwheel={onWheel}
      role="img"
      aria-label="Code graph canvas"
    ></canvas>
  </div>

  {#if selectedNode}
    <div
      class="shrink-0 border-t border-border/50 px-2.5 py-1.5 text-[11px] text-textMuted"
      data-testid="code-graph-selection"
    >
      <span class="font-mono text-textPrimary">{selectedNode.name}</span>
      {#if selectedNode.kind}
        <span class="ml-2">{selectedNode.kind}</span>
      {/if}
      {#if selectedNode.path}
        <span class="ml-2 font-mono truncate">{selectedNode.path}</span>
      {/if}
      {#if selectedNode.community}
        <span class="ml-2">{selectedNode.community}</span>
      {/if}
    </div>
  {/if}
</div>
