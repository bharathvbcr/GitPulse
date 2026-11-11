/**
 * The map canvas and its chrome: toolbar, filters, group chips, node list,
 * connections panel, inspector and zoom controls.
 *
 * A vanilla-DOM port of GitPulse's `CodeGraphCanvas.svelte`, driven by the same
 * pure modules (`graphPayload`, `graphNavigation`, `globeLayout`, the
 * renderer), so the standalone page and the app lay out, filter, trace and
 * paint identically. Differences that are deliberate: the page is read-only, so
 * "open in editor" is "copy path"; and the language palette is injected.
 */

import {
  DEFAULT_CODE_GRAPH_THEME,
  getCodeGraphColor,
  hitTestCodeGraph,
  paintCodeGraph,
  type CodeGraphGlobeFrame,
  type CodeGraphTheme,
} from "../codeGraphRenderer";
import {
  buildCodeGraphModel,
  CODE_GRAPH_LAYOUT_HEIGHT,
  CODE_GRAPH_LAYOUT_WIDTH,
  graphLanguageLegend,
  truncationLegend,
  type CodeGraphModel,
  type LaidOutNode,
} from "../graphPayload";
import {
  clampPitch,
  layoutOnSphere,
  lerpRotation,
  projectOrthographic,
  rotateByDrag,
  rotationToFace,
  type GlobeView,
  type ProjectedPoint,
} from "../globeLayout";
import { buildGraphIndex, filterGraphNodes, fitGraphView, graphNodeOpenPath, traceGraph, type TraceDirection } from "../graphNavigation";
import type { LanguageColorFor, LanguageKey } from "../languages";
import { defaultLanguageColor } from "../languages";
import { copyToClipboard } from "./clipboard";
import { append, button, h, replaceChildren, type Child } from "./dom";
import { easeOutCubic, onSchemeChange, prefersLight, prefersReducedMotion, themeFromCss } from "./theme";

export interface MapViewOptions {
  /** The payload `buildCodeGraphModel` reads (`viz` or `map_preview`). */
  payload: unknown;
  /** Page title shown above the counts. */
  title: string;
  /** Label for the small caption above the title ("Dependency map"). */
  caption: string;
  /** `[flag, label]` filters: show only nodes carrying the flag. */
  flagFilters?: ReadonlyArray<readonly [string, string]>;
  /** Extra `[label, value]` rows for the inspector of a node. */
  detailFor?: (node: LaidOutNode) => ReadonlyArray<readonly [string, string]>;
  colorFor?: LanguageColorFor;
  /** Appended to the honesty line when the view is a capped sample. */
  capHint?: string;
  /** Ring meanings drawn under the toolbar: `[css colour variable, label]`, plus a closing sentence. */
  ringLegend?: { rings: ReadonlyArray<readonly [string, string]>; note: string };
  /** Text shown when the payload has no nodes at all. */
  emptyHint?: string;
  /** Called after the selection changes (id, or null when cleared). */
  onSelect?: (id: string | null) => void;
}

export interface MapViewHandle {
  element: HTMLElement;
  model: CodeGraphModel;
  /** Select a node and bring it to the middle of the view, clearing any filter that hides it. */
  focusNode(id: string): boolean;
  hasNode(id: string): boolean;
  /** Re-measure after the host resized or re-laid-out the view. */
  measure(): void;
  destroy(): void;
}

const FOCUS_TWEEN_MS = 280;
const KEY_ROTATION = (10 * Math.PI) / 180;
const GLOBE_ZOOM_MIN = 0.4;
const GLOBE_ZOOM_MAX = 4;
const FLAT_ZOOM_MIN = 0.000001;
const FLAT_ZOOM_MAX = 5;
const RESULT_PAGE = 30;
const CONNECTION_PAGE = 20;
const GROUP_PAGE = 40;

/** Globe or flat survives a rebuild of the view for the rest of the page's life. */
let globeChoice = false;

function labelFor(node: LaidOutNode): string {
  return node.label || node.name || node.id;
}

function pathText(node: { path?: string; id: string }): string {
  return node.path || node.id;
}

export function createMapView(options: MapViewOptions): MapViewHandle {
  const model = buildCodeGraphModel(options.payload, CODE_GRAPH_LAYOUT_WIDTH, CODE_GRAPH_LAYOUT_HEIGHT);
  const index = buildGraphIndex(model);
  const legend = truncationLegend(model.counts, readCoverage(options.payload));
  const languages = graphLanguageLegend(model.nodes);
  const nodeOrder = new Map(model.nodes.map((node, i) => [node.id, i]));
  const colorFor = options.colorFor ?? defaultLanguageColor;
  const flagFilters = options.flagFilters ?? [];
  const globeAvailable = /^(?:file|symbol)s?$/.test(model.level ?? "");

  // ---- state ---------------------------------------------------------------
  let width = 640;
  let height = 420;
  let panX = 0;
  let panY = 0;
  let scale = 1;
  let globeOn = globeChoice;
  let yaw = 0;
  let pitch = 0;
  let globeZoom = 1;
  let selectedId: string | null = null;
  let hoveredId: string | null = null;
  let query = "";
  let activeCommunity: string | null = null;
  let selectedLanguage: LanguageKey | null = null;
  let hideTests = false;
  let hideGenerated = false;
  let connectedOnly = false;
  let hideNotes = false;
  const requiredFlags = new Set<string>();
  let showLabels = true;
  let showNodeList = false;
  let filtersOpen = false;
  let page = 0;
  let connectionPage = 0;
  let communityPage = 0;
  let direction: TraceDirection = "both";
  let depth = 1;
  let dragging = false;
  let dragDistance = 0;
  let pointerStart = { x: 0, y: 0 };
  let lastPointer = { x: 0, y: 0 };
  let tweenFrame = 0;
  let paintFrame = 0;
  let hoverFrame = 0;
  let light = prefersLight();
  let theme: CodeGraphTheme = DEFAULT_CODE_GRAPH_THEME;
  let themeFor: boolean | null = null;
  let matches: LaidOutNode[] = model.nodes;
  let visibleIds: ReadonlySet<string> = new Set(model.nodes.map((n) => n.id));
  let destroyed = false;
  const projectionScratch: ProjectedPoint[] = [];
  let unitPoints: ReturnType<typeof layoutOnSphere> = [];
  const cleanups: Array<() => void> = [];

  const globeActive = (): boolean => globeOn && globeAvailable;

  // ---- elements ------------------------------------------------------------
  const counts = h("div", { class: "cm-counts", attrs: { role: "status", "data-testid": "code-graph-legend" } });
  const clearFiltersBtn = button("Clear filters", () => clearFilters(), { class: "cm-btn cm-btn-small", attrs: { "data-testid": "map-clear-filters" } });
  const searchInput = h("input", {
    class: "cm-search-input",
    attrs: { type: "search", "aria-label": "Find a file or symbol", placeholder: "Find a file or symbol…", autocomplete: "off", spellcheck: "false" },
    on: {
      input: () => { query = searchInput.value; filtersChanged(); },
      keydown: (e) => { if (e.key === "Enter" && matches[0]) focusNodeById(matches[0].id); },
    },
  });
  const nodesBtn = button("Nodes", () => { showNodeList = !showNodeList; refresh(); }, { class: "cm-btn", attrs: { "data-testid": "map-nodes-toggle" } });
  const labelsBtn = button("Labels", () => { showLabels = !showLabels; refresh(); }, { class: "cm-btn" });
  const flatBtn = button("Flat", () => setGlobe(false), { class: "cm-seg" });
  const globeBtn = button("Globe", () => setGlobe(true), { class: "cm-seg" });
  const layout = h("div", { class: "cm-segmented", attrs: { role: "group", "aria-label": "Layout", "data-testid": "map-layout" } }, flatBtn, globeBtn);
  const filtersBtn = button("Filters", () => { filtersOpen = !filtersOpen; refresh(); }, {
    class: "cm-btn",
    attrs: { "aria-haspopup": "true", "aria-expanded": "false", "data-testid": "map-filters-toggle" },
  });
  const filtersPanel = h("div", { class: "cm-menu", attrs: { role: "group", "aria-label": "Graph filters" } });
  const filtersWrap = h("div", { class: "cm-filters" }, filtersBtn, filtersPanel);
  const note = h("p", { class: "cm-note", attrs: { role: "note", "data-testid": "code-graph-truncation" } });
  const legendEl = h("div", { class: "cm-ring-legend", attrs: { "aria-label": "Ring legend" } });
  const warnings = h("p", { class: "cm-note", attrs: { role: "note" } });
  const groupChips = h("div", { class: "cm-chips", attrs: { "aria-label": "Communities" } });
  const groupPager = h("div", { class: "cm-pager" });
  const groupRow = h("div", { class: "cm-group-row" }, groupChips, groupPager);
  const canvas = h("canvas", { class: "cm-canvas", attrs: { tabindex: "0", role: "application" } });
  const results = h("div", { class: "cm-panel cm-results", attrs: { role: "region" } });
  const neighbors = h("aside", { class: "cm-panel cm-neighbors", attrs: { "aria-label": "Node connections" } });
  const empty = h("div", { class: "cm-empty" });
  const zoomReadout = h("span", { class: "cm-zoom-readout" });
  const zoomOutBtn = button("−", () => zoomAt(1 / 1.2), { class: "cm-icon-btn", attrs: { "aria-label": "Zoom out" } });
  const zoomInBtn = button("+", () => zoomAt(1.2), { class: "cm-icon-btn", attrs: { "aria-label": "Zoom in" } });
  const fitBtn = button("Fit view", () => resetView(), { class: "cm-btn", attrs: { "data-testid": "map-fit" } });
  const zoom = h("div", { class: "cm-zoom", attrs: { role: "group", "aria-label": "Graph view controls" } }, zoomOutBtn, zoomReadout, zoomInBtn, fitBtn);
  const help = h("div", { class: "cm-help" });
  const live = h("div", { class: "cm-live", attrs: { role: "status", "aria-live": "polite" } });
  const stage = h("div", { class: "cm-stage" }, canvas, results, neighbors, empty, zoom, help, live);
  const inspector = h("div", { class: "cm-inspector", attrs: { "data-testid": "code-graph-selection" } });
  const element = h(
    "section",
    { class: "cm", attrs: { "aria-label": options.caption } },
    h(
      "div",
      { class: "cm-toolbar" },
      h("div", { class: "cm-heading" }, h("span", { class: "cm-caption", text: options.caption }), h("h1", { class: "cm-title", text: options.title }), counts),
      h(
        "div",
        { class: "cm-actions" },
        h("label", { class: "cm-search" }, searchInput),
        nodesBtn,
        labelsBtn,
        globeAvailable ? layout : null,
        filtersWrap,
      ),
    ),
    note,
    warnings,
    legendEl,
    groupRow,
    stage,
    inspector,
  );

  // ---- derived -------------------------------------------------------------
  function recompute(): void {
    const base = filterGraphNodes(model, index, {
      query, community: activeCommunity, language: selectedLanguage, hideTests, hideGenerated, hideNotes, connectedOnly,
    });
    matches = requiredFlags.size === 0
      ? base
      : base.filter((node) => [...requiredFlags].every((flag) => (node.flags ?? []).some((f) => f.flag === flag)));
    visibleIds = new Set(matches.map((n) => n.id));
    if (globeActive() && unitPoints.length === 0) unitPoints = layoutOnSphere(model);
  }

  const filtersActive = (): boolean =>
    Boolean(query || activeCommunity || selectedLanguage || hideTests || hideGenerated || hideNotes || connectedOnly || requiredFlags.size);

  function currentTheme(): CodeGraphTheme {
    if (themeFor !== light) {
      theme = themeFromCss(element);
      themeFor = light;
    }
    return theme;
  }

  // ---- painting ------------------------------------------------------------
  function globeRadius(): number {
    return Math.max(40, Math.min(width, height) / 2 - 28);
  }

  function globeView(): GlobeView {
    return { yaw, pitch, zoom: globeZoom, cx: width / 2, cy: height / 2, radius: globeRadius() };
  }

  function globeFrame(): CodeGraphGlobeFrame | null {
    if (!globeActive()) return null;
    if (unitPoints.length === 0) unitPoints = layoutOnSphere(model);
    const view = globeView();
    return { projection: projectOrthographic(unitPoints, view, projectionScratch), cx: view.cx, cy: view.cy, radius: view.radius * view.zoom };
  }

  function schedulePaint(): void {
    if (paintFrame || destroyed) return;
    paintFrame = requestAnimationFrame(() => {
      paintFrame = 0;
      paint();
    });
  }

  function paint(): void {
    const ctx = canvas.getContext("2d");
    if (!ctx || destroyed) return;
    const dpr = window.devicePixelRatio || 1;
    const wantW = Math.max(1, Math.round(width * dpr));
    const wantH = Math.max(1, Math.round(height * dpr));
    if (canvas.width !== wantW || canvas.height !== wantH) {
      canvas.width = wantW;
      canvas.height = wantH;
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const trace = selectedId || hoveredId ? traceGraph(index, selectedId || hoveredId || "", direction, depth, visibleIds) : null;
    paintCodeGraph(ctx, {
      model, widthCss: width, heightCss: height, panX, panY, scale, selectedId, hoveredId, visibleIds,
      traceIds: trace?.ids, traceEdges: trace?.edges, showLabels, light, colorFor,
      theme: currentTheme(), activeCommunity, globe: globeFrame(),
    });
  }

  function measure(): void {
    const rect = stage.getBoundingClientRect();
    const nextW = Math.max(160, Math.floor(rect.width));
    const nextH = Math.max(160, Math.floor(rect.height));
    if (nextW === width && nextH === height) return;
    width = nextW;
    height = nextH;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    schedulePaint();
  }

  // ---- view control --------------------------------------------------------
  function cancelTween(): void {
    if (tweenFrame) cancelAnimationFrame(tweenFrame);
    tweenFrame = 0;
  }

  function zoomAt(factor: number, x = width / 2, y = height / 2): void {
    if (globeActive()) {
      globeZoom = Math.min(GLOBE_ZOOM_MAX, Math.max(GLOBE_ZOOM_MIN, globeZoom * factor));
    } else {
      const next = Math.min(FLAT_ZOOM_MAX, Math.max(FLAT_ZOOM_MIN, scale * factor));
      panX = x - ((x - panX) * next) / scale;
      panY = y - ((y - panY) * next) / scale;
      scale = next;
    }
    refreshZoom();
    schedulePaint();
  }

  function resetFlatView(): void {
    const fitted = fitGraphView(matches, width, height);
    panX = fitted.panX;
    panY = fitted.panY;
    scale = fitted.scale;
  }

  function resetGlobeView(): void {
    yaw = 0;
    pitch = 0;
    globeZoom = 1;
  }

  function resetView(): void {
    cancelTween();
    if (globeActive()) resetGlobeView();
    else resetFlatView();
    refreshZoom();
    schedulePaint();
  }

  function faceNode(id: string): void {
    const at = nodeOrder.get(id);
    const point = at === undefined ? undefined : unitPoints[at];
    if (!point) return;
    cancelTween();
    const target = rotationToFace(point);
    if (prefersReducedMotion()) {
      yaw = target.yaw;
      pitch = target.pitch;
      schedulePaint();
      return;
    }
    const from = { yaw, pitch };
    const began = performance.now();
    const step = (now: number): void => {
      const t = Math.min(1, (now - began) / FOCUS_TWEEN_MS);
      const turned = lerpRotation(from, target, easeOutCubic(t));
      yaw = turned.yaw;
      pitch = turned.pitch;
      schedulePaint();
      tweenFrame = t < 1 ? requestAnimationFrame(step) : 0;
    };
    tweenFrame = requestAnimationFrame(step);
  }

  function focusNodeById(id: string): void {
    const node = index.nodes.get(id);
    if (!node || !visibleIds.has(id)) return;
    selectedId = id;
    query = "";
    searchInput.value = "";
    showNodeList = false;
    connectionPage = 0;
    if (globeActive()) {
      faceNode(id);
    } else {
      scale = Math.max(1.5, scale);
      panX = (width > 760 ? (width - 340) / 2 : width / 2) - node.x * scale;
      panY = height * (width > 760 ? 0.5 : 0.65) - node.y * scale;
    }
    recompute();
    refresh();
    options.onSelect?.(id);
  }

  function setGlobe(on: boolean): void {
    cancelTween();
    globeOn = on;
    globeChoice = on;
    hoveredId = null;
    if (on && globeAvailable) {
      recompute();
      if (selectedId) faceNode(selectedId);
    }
    refresh();
  }

  function select(id: string | null): void {
    if (selectedId === id) return;
    selectedId = id;
    connectionPage = 0;
    refresh();
    options.onSelect?.(id);
  }

  function filtersChanged(): void {
    const hadSelection = selectedId !== null;
    selectedId = null;
    hoveredId = null;
    page = 0;
    connectionPage = 0;
    recompute();
    refresh();
    if (hadSelection) options.onSelect?.(null);
  }

  function clearFilters(): void {
    query = "";
    searchInput.value = "";
    activeCommunity = null;
    selectedLanguage = null;
    hideTests = false;
    hideGenerated = false;
    hideNotes = false;
    connectedOnly = false;
    requiredFlags.clear();
    filtersChanged();
  }

  async function copyPath(node: LaidOutNode): Promise<void> {
    const path = graphNodeOpenPath(node);
    if (!path) return;
    const ok = await copyToClipboard(path);
    live.textContent = ok ? `Copied ${path}` : `Could not copy ${path}; select it from the inspector instead.`;
  }

  // ---- pointer + keyboard --------------------------------------------------
  function hit(clientX: number, clientY: number): ReturnType<typeof hitTestCodeGraph> {
    const rect = canvas.getBoundingClientRect();
    return hitTestCodeGraph(model, clientX - rect.left, clientY - rect.top, panX, panY, scale, visibleIds, globeFrame());
  }

  canvas.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    canvas.focus();
    canvas.setPointerCapture(e.pointerId);
    if (hoverFrame) cancelAnimationFrame(hoverFrame);
    hoverFrame = 0;
    cancelTween();
    dragging = true;
    lastPointer = { x: e.clientX, y: e.clientY };
    pointerStart = lastPointer;
    dragDistance = 0;
  });
  canvas.addEventListener("pointermove", (e) => {
    const pt = { x: e.clientX, y: e.clientY };
    if (dragging) {
      dragDistance = Math.max(dragDistance, Math.hypot(pt.x - pointerStart.x, pt.y - pointerStart.y));
      if (globeActive()) {
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
    // One hit test per frame, from the newest pointer position.
    if (hoverFrame) cancelAnimationFrame(hoverFrame);
    hoverFrame = requestAnimationFrame(() => {
      hoverFrame = 0;
      if (dragging || destroyed) return;
      const found = hit(pt.x, pt.y);
      const next = found?.kind === "node" ? found.id : null;
      if (next !== hoveredId) {
        hoveredId = next;
        canvas.style.cursor = next ? "pointer" : "";
        refreshInspector();
        schedulePaint();
      }
    });
  });
  const endDrag = (e: PointerEvent): void => {
    dragging = false;
    try {
      canvas.releasePointerCapture(e.pointerId);
    } catch {
      // Cancellation can release capture before this handler runs.
    }
  };
  canvas.addEventListener("pointerup", endDrag);
  canvas.addEventListener("pointercancel", endDrag);
  canvas.addEventListener("pointerleave", () => {
    if (dragging) return;
    hoveredId = null;
    refreshInspector();
    schedulePaint();
  });
  canvas.addEventListener("click", (e) => {
    if (dragDistance > 4) return;
    const found = hit(e.clientX, e.clientY);
    select(found?.kind === "node" ? found.id : null);
  });
  canvas.addEventListener("dblclick", (e) => {
    const found = hit(e.clientX, e.clientY);
    if (found?.kind === "node" && found.node) void copyPath(found.node);
  });
  canvas.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      if (e.deltaY === 0) return;
      const rect = canvas.getBoundingClientRect();
      zoomAt(Math.exp(-Math.max(-120, Math.min(120, e.deltaY)) * 0.002), e.clientX - rect.left, e.clientY - rect.top);
    },
    { passive: false },
  );
  canvas.addEventListener("keydown", (e) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.key === "Escape") {
      clearFilters();
      showNodeList = false;
      refresh();
    } else if (e.key === "+" || e.key === "=") zoomAt(1.2);
    else if (e.key === "-") zoomAt(1 / 1.2);
    else if (e.key === "0") resetView();
    else if (e.key === "Enter") {
      const node = selectedId ? index.nodes.get(selectedId) : undefined;
      if (node) void copyPath(node);
    } else if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) {
      if (globeActive()) {
        cancelTween();
        yaw += e.key === "ArrowLeft" ? KEY_ROTATION : e.key === "ArrowRight" ? -KEY_ROTATION : 0;
        pitch = clampPitch(pitch + (e.key === "ArrowUp" ? KEY_ROTATION : e.key === "ArrowDown" ? -KEY_ROTATION : 0));
      } else {
        panX += e.key === "ArrowLeft" ? 40 : e.key === "ArrowRight" ? -40 : 0;
        panY += e.key === "ArrowUp" ? 40 : e.key === "ArrowDown" ? -40 : 0;
      }
      schedulePaint();
    } else return;
    e.preventDefault();
    schedulePaint();
  });
  canvas.addEventListener("contextlost", (e) => { e.preventDefault(); });
  canvas.addEventListener("contextrestored", () => schedulePaint());

  // Escape closes the filters popover from anywhere inside it; a click outside does too.
  const onDocumentPointerDown = (e: PointerEvent): void => {
    if (filtersOpen && e.target instanceof Node && !filtersWrap.contains(e.target)) {
      filtersOpen = false;
      refreshFilters();
    }
  };
  document.addEventListener("pointerdown", onDocumentPointerDown);
  cleanups.push(() => document.removeEventListener("pointerdown", onDocumentPointerDown));
  filtersWrap.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && filtersOpen) {
      e.stopPropagation();
      filtersOpen = false;
      refreshFilters();
      filtersBtn.focus();
    }
  });

  // ---- panels --------------------------------------------------------------
  function pressed(btn: HTMLElement, on: boolean): void {
    btn.setAttribute("aria-pressed", String(on));
    btn.classList.toggle("is-on", on);
  }

  function dot(node: LaidOutNode): HTMLElement {
    const el = h("span", { class: "cm-dot", attrs: { "aria-hidden": "true" } });
    el.style.background = getCodeGraphColor(node, light, colorFor);
    return el;
  }

  function nodeRow(node: LaidOutNode, extra: Child[] = []): HTMLButtonElement {
    const row = button("", () => focusNodeById(node.id), { class: "cm-row", title: pathText(node), attrs: { "data-node-id": node.id } });
    if (selectedId === node.id) row.classList.add("is-on");
    append(row, [dot(node), h("span", { class: "cm-row-copy" }, h("strong", { text: labelFor(node) }), h("small", { text: pathText(node) }), ...extra)]);
    return row;
  }

  function pagination(current: number, total: number, label: string, set: (n: number) => void): HTMLElement {
    return h(
      "div",
      { class: "cm-pagination" },
      button("Previous", () => set(current - 1), { class: "cm-btn cm-btn-small", attrs: { "aria-label": `Previous ${label}`, disabled: current === 0 } }),
      h("span", { text: `${current + 1} / ${total}` }),
      button("Next", () => set(current + 1), { class: "cm-btn cm-btn-small", attrs: { "aria-label": `Next ${label}`, disabled: current + 1 >= total } }),
    );
  }

  function refreshCounts(): void {
    const parts: Child[] = [];
    parts.push(h("span", { text: legend.nodesLabel, attrs: { "data-testid": "code-graph-nodes-label" } }));
    if (legend.linksLabel) parts.push(h("span", { text: `· ${legend.linksLabel}` }));
    parts.push(h("span", { text: `· ${model.communities.length} groups` }));
    parts.push(h("span", { text: `· ${matches.length} of ${model.nodes.length} match filters`, attrs: { "data-testid": "graph-filter-count" } }));
    if (filtersActive()) parts.push(clearFiltersBtn);
    replaceChildren(counts, ...parts);
    const lines = [legend.honesty, legend.nodesTruncated ? options.capHint : null, legend.coverageLabel].filter((line): line is string => Boolean(line));
    note.textContent = lines.join(" ");
    note.hidden = lines.length === 0;
    warnings.textContent = model.warnings.join(" ");
    warnings.hidden = model.warnings.length === 0;
  }

  function refreshLegend(): void {
    const legendSpec = options.ringLegend;
    legendEl.hidden = !legendSpec;
    if (!legendSpec) return;
    replaceChildren(
      legendEl,
      ...legendSpec.rings.map(([variable, label]) => {
        const ring = h("span", { class: "cm-ring", attrs: { "aria-hidden": "true" } });
        ring.style.borderColor = `var(${variable})`;
        return h("span", { class: "cm-ring-item" }, ring, label);
      }),
      h("span", { class: "cm-muted", text: legendSpec.note }),
    );
  }

  function refreshToolbar(): void {
    pressed(nodesBtn, showNodeList);
    pressed(labelsBtn, showLabels);
    pressed(flatBtn, !globeActive());
    pressed(globeBtn, globeActive());
    filtersBtn.classList.toggle("is-on", filtersActive());
  }

  function checkbox(label: string, title: string, checked: boolean, set: (v: boolean) => void, name?: string): HTMLLabelElement {
    const input = h("input", { attrs: { type: "checkbox", checked, "aria-label": name } });
    input.checked = checked;
    input.addEventListener("change", () => { set(input.checked); filtersChanged(); });
    return h("label", { class: "cm-menu-item", title }, input, h("span", { text: label }));
  }

  function refreshFilters(): void {
    filtersBtn.setAttribute("aria-expanded", String(filtersOpen));
    filtersPanel.hidden = !filtersOpen;
    if (!filtersOpen) return;
    const items: Child[] = [
      checkbox("Hide tests", "Identified by conventional test directory and filename patterns", hideTests, (v) => { hideTests = v; }),
      checkbox("Hide generated/vendor", "Identified by conventional generated, build, and vendor paths", hideGenerated, (v) => { hideGenerated = v; }),
      checkbox("Connected only", "Keep nodes with relationships in the loaded graph", connectedOnly, (v) => { connectedOnly = v; }),
      checkbox("Hide notes & Markdown", "Hide document and note nodes, or subsystems made entirely of documentation", hideNotes, (v) => { hideNotes = v; }, "Hide notes & Markdown"),
    ];
    for (const [flag, label] of flagFilters) {
      items.push(checkbox(label, `Show only nodes flagged ${flag}`, requiredFlags.has(flag), (v) => { if (v) requiredFlags.add(flag); else requiredFlags.delete(flag); }));
    }
    if (languages.length > 0) {
      const langButtons: Child[] = [
        button("All languages", () => { selectedLanguage = null; filtersChanged(); }, { class: "cm-menu-item", attrs: { "aria-pressed": String(!selectedLanguage), "data-map-language": "" } }),
      ];
      for (const language of languages) {
        const swatch = h("span", { class: "cm-dot", attrs: { "aria-hidden": "true" } });
        swatch.style.background = colorFor(language.key, light ? "light" : "dark");
        const item = button("", () => { selectedLanguage = selectedLanguage === language.key ? null : language.key; filtersChanged(); }, {
          class: "cm-menu-item",
          title: `${language.count} nodes in loaded graph`,
          attrs: { "aria-pressed": String(selectedLanguage === language.key), "data-map-language": language.key },
        });
        append(item, [swatch, h("span", { class: "cm-grow", text: language.name }), h("span", { class: "cm-count", text: String(language.count) })]);
        langButtons.push(item);
      }
      items.push(h("div", { class: "cm-menu-sep", attrs: { role: "separator" } }), h("div", { attrs: { role: "group", "aria-label": "Node colors by language" } }, ...langButtons));
    }
    replaceChildren(filtersPanel, ...items);
  }

  function refreshGroups(): void {
    groupRow.hidden = model.communities.length === 0;
    if (model.communities.length === 0) return;
    const pages = Math.max(1, Math.ceil(model.communities.length / GROUP_PAGE));
    communityPage = Math.min(communityPage, pages - 1);
    const chips: Child[] = [
      button("All groups", () => { activeCommunity = null; filtersChanged(); }, { class: "cm-chip", attrs: { "aria-pressed": String(!activeCommunity), "data-map-group": "" } }),
    ];
    for (const community of model.communities.slice(communityPage * GROUP_PAGE, (communityPage + 1) * GROUP_PAGE)) {
      const chip = button("", () => { activeCommunity = activeCommunity === community.name ? null : community.name; filtersChanged(); }, {
        class: "cm-chip",
        title: `${community.name} · ${community.shown} shown of ${community.count}`,
        attrs: { "aria-pressed": String(activeCommunity === community.name), "data-map-group": community.name },
      });
      append(chip, [h("span", { class: "cm-trunc", text: community.label }), h("span", { class: "cm-count", text: String(community.shown) })]);
      chips.push(chip);
    }
    replaceChildren(groupChips, ...chips);
    for (const chip of groupChips.children) chip.classList.toggle("is-on", chip.getAttribute("aria-pressed") === "true");
    replaceChildren(
      groupPager,
      ...(pages > 1
        ? [
            button("‹", () => { communityPage = Math.max(0, communityPage - 1); refreshGroups(); }, { class: "cm-icon-btn", attrs: { "aria-label": "Previous groups", disabled: communityPage === 0 } }),
            h("span", { text: `${communityPage + 1}/${pages}` }),
            button("›", () => { communityPage = Math.min(pages - 1, communityPage + 1); refreshGroups(); }, { class: "cm-icon-btn", attrs: { "aria-label": "Next groups", disabled: communityPage + 1 >= pages } }),
          ]
        : []),
    );
  }

  function refreshResults(): void {
    const searching = query.trim().length > 0;
    results.hidden = !(searching || showNodeList);
    if (results.hidden) return;
    const pages = Math.max(1, Math.ceil(matches.length / RESULT_PAGE));
    page = Math.min(page, pages - 1);
    const from = matches.length ? page * RESULT_PAGE + 1 : 0;
    const to = Math.min(matches.length, (page + 1) * RESULT_PAGE);
    results.setAttribute("aria-label", searching ? "Search results" : "Node browser");
    replaceChildren(
      results,
      h("div", { class: "cm-panel-head", text: `${matches.length} ${searching ? "matches" : "nodes"} · ${from}–${to} shown` }),
      ...matches.slice(page * RESULT_PAGE, (page + 1) * RESULT_PAGE).map((node) => nodeRow(node)),
      matches.length === 0 ? h("p", { class: "cm-panel-note", text: "No nodes match the current filters." }) : null,
      pages > 1 ? pagination(page, pages, "nodes", (n) => { page = n; refreshResults(); }) : null,
    );
  }

  function refreshNeighbors(): void {
    const selected = selectedId ? index.nodes.get(selectedId) : undefined;
    neighbors.hidden = !selected || !results.hidden;
    if (!selected || !results.hidden) return;
    const trace = traceGraph(index, selected.id, direction, depth, visibleIds);
    const rows = (["outgoing", "incoming", "related"] as const).flatMap((side) =>
      (index[side].get(selected.id) ?? []).flatMap((edgeIndex) => {
        const link = model.links[edgeIndex];
        const other = index.nodes.get(side === "related" ? (link.source === selected.id ? link.target : link.source) : side === "outgoing" ? link.target : link.source);
        return other ? [{ side, edgeIndex, node: other, link }] : [];
      }),
    );
    const shownRows = rows.filter((row) => visibleIds.has(row.node.id) && (row.side === "related" || direction === "both" || row.side === direction));
    const pages = Math.max(1, Math.ceil(shownRows.length / CONNECTION_PAGE));
    connectionPage = Math.min(connectionPage, pages - 1);
    const dirSelect = h(
      "select",
      { class: "cm-select", attrs: { "aria-label": "Trace direction" }, on: { change: () => { direction = dirSelect.value === "outgoing" || dirSelect.value === "incoming" ? dirSelect.value : "both"; connectionPage = 0; schedulePaint(); refreshNeighbors(); } } },
      h("option", { text: "Both directions", attrs: { value: "both" } }),
      h("option", { text: "Depends on", attrs: { value: "outgoing" } }),
      h("option", { text: "Used by", attrs: { value: "incoming" } }),
    );
    dirSelect.value = direction;
    const depthSelect = h(
      "select",
      { class: "cm-select", attrs: { "aria-label": "Trace depth" }, on: { change: () => { depth = Number(depthSelect.value) || 1; schedulePaint(); refreshNeighbors(); } } },
      ...[1, 2, 3].map((n) => h("option", { text: `${n} hop${n === 1 ? "" : "s"}`, attrs: { value: n } })),
    );
    depthSelect.value = String(depth);
    replaceChildren(
      neighbors,
      h("div", { class: "cm-panel-head cm-between" }, h("strong", { text: "Connections" }), button("×", () => select(null), { class: "cm-icon-btn", attrs: { "aria-label": "Close connections" } })),
      h("div", { class: "cm-trace" }, dirSelect, depthSelect),
      h("p", { class: "cm-panel-note", attrs: { role: "status" }, text: `${trace.ids.size} nodes in trace · ${shownRows.length} of ${rows.length} direct relationships match filters/direction` }),
      trace.truncated ? h("p", { class: "cm-note", attrs: { role: "note" }, text: "Trace limit reached (1,000 nodes or 50,000 edge visits). More nodes may be reachable." }) : null,
      ...shownRows.slice(connectionPage * CONNECTION_PAGE, (connectionPage + 1) * CONNECTION_PAGE).map((row) =>
        nodeRow(row.node, [
          h("small", { text: `${row.side === "related" ? "Related to" : row.side === "outgoing" ? "Depends on" : "Used by"} · ${row.link.kind || "relationship"}` }),
          h("small", { text: `${row.link.evidence_count ?? 1} source relationship(s) · ${row.link.confidence == null ? "confidence unknown" : `${Math.round(row.link.confidence * 100)}% confidence`}` }),
        ]),
      ),
      shownRows.length === 0 ? h("p", { class: "cm-panel-note", text: "No direct relationships match this view." }) : null,
      pages > 1 ? pagination(connectionPage, pages, "connections", (n) => { connectionPage = n; refreshNeighbors(); }) : null,
    );
  }

  function refreshInspector(): void {
    const selected = selectedId ? index.nodes.get(selectedId) : undefined;
    const node = selected ?? (hoveredId ? index.nodes.get(hoveredId) : undefined);
    if (!node) {
      replaceChildren(inspector, h("span", { class: "cm-muted cm-grow", text: "Select a node to inspect its file and connections." }), h("span", { class: "cm-muted", text: "Node size reflects connectivity" }));
      return;
    }
    const connected = new Set([...(index.outgoing.get(node.id) ?? []), ...(index.incoming.get(node.id) ?? []), ...(index.related.get(node.id) ?? [])]).size;
    const details = (options.detailFor?.(node) ?? []).filter(([, value]) => value);
    const open = graphNodeOpenPath(node);
    replaceChildren(
      inspector,
      dot(node),
      h("div", { class: "cm-inspector-copy" }, h("strong", { text: labelFor(node), title: pathText(node) }), h("span", { text: pathText(node), title: pathText(node) })),
      h("span", { class: "cm-muted", text: node.kind ?? model.level ?? "node" }),
      selected ? h("span", { class: "cm-muted", text: `${connected} loaded connections` }) : null,
      node.flags?.length ? h("span", { class: "cm-muted", text: node.flags.map((f) => f.flag).join(" · ") }) : null,
      ...details.map(([label, value]) => h("span", { class: "cm-muted cm-detail", text: `${label}: ${value}`, title: `${label}: ${value}` })),
      open ? button("Copy path", () => void copyPath(node), { class: "cm-btn" }) : null,
      selected ? button("×", () => select(null), { class: "cm-icon-btn", attrs: { "aria-label": "Clear selection" } }) : null,
    );
  }

  function refreshEmpty(): void {
    const none = model.nodes.length === 0;
    const filtered = !none && matches.length === 0;
    empty.hidden = !(none || filtered);
    if (empty.hidden) return;
    replaceChildren(
      empty,
      h("strong", { text: none ? "No nodes to display" : "No nodes match the current filters" }),
      h("span", { text: none ? options.emptyHint ?? "This graph payload is empty." : "Clear filters to show all loaded nodes." }),
    );
  }

  function refreshZoom(): void {
    const value = globeActive() ? globeZoom : scale;
    const min = globeActive() ? GLOBE_ZOOM_MIN : FLAT_ZOOM_MIN;
    const max = globeActive() ? GLOBE_ZOOM_MAX : FLAT_ZOOM_MAX;
    zoomReadout.textContent = `${value < 0.01 ? "<1" : Math.round(value * 100)}%`;
    zoomOutBtn.disabled = value <= min;
    zoomInBtn.disabled = value >= max;
    fitBtn.textContent = globeActive() ? "Reset view" : "Fit view";
    fitBtn.title = globeActive() ? "Face the globe front-on (0)" : "Fit filtered nodes into the view (0)";
    help.textContent = `${globeActive() ? "Drag to rotate" : "Drag to explore"} · Scroll to zoom · Select to trace connections`;
    canvas.setAttribute(
      "aria-label",
      globeActive()
        ? "Code map, globe view. Drag to rotate, scroll to zoom. Arrow keys rotate; plus and minus zoom; Enter copies the selected path; Escape clears."
        : "Code map. Drag to pan, scroll to zoom. Arrow keys pan; plus and minus zoom; Enter copies the selected path; Escape clears.",
    );
  }

  function refresh(): void {
    if (destroyed) return;
    refreshCounts();
    refreshLegend();
    refreshToolbar();
    refreshFilters();
    refreshGroups();
    refreshResults();
    refreshNeighbors();
    refreshInspector();
    refreshEmpty();
    refreshZoom();
    schedulePaint();
  }

  // ---- test surface --------------------------------------------------------
  // Read-only: a function a browser test calls on the canvas to ask where the
  // camera is and where each shown node landed on screen, so `page_probe.cjs`
  // can assert fit, filtering and the globe against real geometry rather than
  // pixels. It changes no state, draws nothing and has no UI.
  Object.defineProperty(canvas, "cmProbe", {
    value: () => {
      const globe = globeFrame();
      const nodes = model.nodes.flatMap((node, i) => {
        if (!visibleIds.has(node.id)) return [];
        const at = globe ? globe.projection[i] : null;
        return [{
          id: node.id,
          x: at ? at.x : node.x * scale + panX,
          y: at ? at.y : node.y * scale + panY,
          r: node.radius * (at ? 1 : scale),
          front: at ? at.front : true,
        }];
      });
      return { globe: globe !== null, width, height, scale, panX, panY, yaw, pitch, zoom: globeZoom, flatZoomMin: FLAT_ZOOM_MIN, flatZoomMax: FLAT_ZOOM_MAX, nodes };
    },
  });

  // ---- lifecycle -----------------------------------------------------------
  const resizeObserver = typeof ResizeObserver === "function" ? new ResizeObserver(() => measure()) : null;
  resizeObserver?.observe(stage);
  cleanups.push(onSchemeChange(() => { light = prefersLight(); themeFor = null; refresh(); }));

  recompute();
  refresh();
  // The first fit needs the real stage size, which exists once the host attaches the element.
  requestAnimationFrame(() => {
    measure();
    resetFlatView();
    refreshZoom();
    schedulePaint();
  });

  return {
    element,
    model,
    focusNode(id) {
      if (!index.nodes.has(id)) return false;
      if (!visibleIds.has(id)) clearFilters();
      focusNodeById(id);
      return true;
    },
    hasNode: (id) => index.nodes.has(id),
    measure,
    destroy() {
      destroyed = true;
      cancelTween();
      if (paintFrame) cancelAnimationFrame(paintFrame);
      if (hoverFrame) cancelAnimationFrame(hoverFrame);
      resizeObserver?.disconnect();
      for (const cleanup of cleanups) cleanup();
      element.remove();
    },
  };
}

function readCoverage(payload: unknown): unknown {
  if (payload === null || typeof payload !== "object") return undefined;
  const meta: unknown = Reflect.get(payload, "meta");
  if (meta === null || typeof meta !== "object") return undefined;
  return Reflect.get(meta, "coverage");
}
