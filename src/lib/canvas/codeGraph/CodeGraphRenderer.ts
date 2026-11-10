/**
 * Code-graph canvas renderer: a free 2D plane, or an opt-in globe.
 *
 * Deliberately separate from the commit `GraphRenderer` (lane solver /
 * VisualCommitRow). Reuses gpuContext + frameScheduler infrastructure only.
 * Hit-test kinds are symbol/file oriented: `node` | `link`.
 */

import { nodeCommunity, nodeLanguageKey } from "../../codeintel/graphPayload";
import { isUndirectedGraphLink } from "../../codeintel/graphNavigation";
import type { CodeGraphModel, LaidOutNode } from "../../codeintel/graphPayload";
import type { ProjectedPoint } from "../../codeintel/globeLayout";
import type { GraphVizNode } from "../../codeintel/types";
import { getLanguageIconColor } from "../../language/languageLogos";

export type CodeGraphHitKind = "node" | "link";

export interface CodeGraphHit {
  kind: CodeGraphHitKind;
  /** Node id, or `source→target` for a link. */
  id: string;
  node?: LaidOutNode;
  linkIndex?: number;
}

/**
 * Every colour the renderer draws with. The host resolves its own tokens into
 * this object (`themeFromCss` in the app); the defaults below are neutral and
 * exist only so a host that supplies nothing still draws something legible.
 */
export interface CodeGraphTheme {
  background: string | null;
  link: string;
  linkMuted: string;
  label: string;
  selection: string;
  hover: string;
  /** Opaque plate behind a label. Falls back to `background`, then this. */
  plate: string;
  /** Community field and globe limb ink; drawn at low alpha. */
  field: string;
  dead: string;
  unwired: string;
  entry: string;
}

export const DEFAULT_CODE_GRAPH_THEME: CodeGraphTheme = {
  background: null,
  link: "rgba(120, 140, 170, 0.45)",
  linkMuted: "rgba(120, 140, 170, 0.22)",
  label: "rgba(180, 190, 210, 0.92)",
  selection: "#3d8bfd",
  hover: "#f0ad4e",
  plate: "#171e2b",
  field: "#94a3b8",
  dead: "#e35d6a",
  unwired: "#f0ad4e",
  entry: "#34d399",
};

/**
 * A globe frame: the host projects the unit-sphere layout for the current
 * rotation and hands the result in screen (CSS pixel) space. Pan and scale do
 * not apply; zoom is already folded into the projection.
 */
export interface CodeGraphGlobeFrame {
  /** One projected point per `model.nodes` entry, in the same order. */
  projection: readonly ProjectedPoint[];
  cx: number;
  cy: number;
  /** Sphere radius on screen, zoom included. */
  radius: number;
}

/** Nodes within this depth of the limb are foreshortened to a sliver: unlabelled. */
export const GLOBE_LABEL_MIN_DEPTH = 0.3;
const GLOBE_BACK_ALPHA = 0.18;

export interface CodeGraphPaintRequest {
  model: CodeGraphModel;
  widthCss: number;
  heightCss: number;
  /** Pan offset in content space. */
  panX: number;
  panY: number;
  scale: number;
  selectedId: string | null;
  hoveredId: string | null;
  theme: CodeGraphTheme;
  /** When true, draw name labels for nodes above a size threshold. */
  showLabels?: boolean;
  matchingIds?: ReadonlySet<string> | null;
  activeCommunity?: string | null;
  /** Hard filters affect both painting and picking. Trace sets only emphasize. */
  visibleIds?: ReadonlySet<string> | null;
  traceIds?: ReadonlySet<string> | null;
  traceEdges?: ReadonlySet<number> | null;
  light?: boolean;
  /** Globe mode: positions come from the projection and pan/scale are ignored. */
  globe?: CodeGraphGlobeFrame | null;
}

/** On-screen radius of a node: depth shrinks it toward the limb. */
function globeRadius(node: LaidOutNode, point: ProjectedPoint): number {
  return node.radius * (point.front ? 0.7 + 0.3 * point.depth : 0.6);
}

function dist2(ax: number, ay: number, bx: number, by: number): number {
  const dx = ax - bx;
  const dy = ay - by;
  return dx * dx + dy * dy;
}

/** Distance from point P to segment AB, in content space. */
function distToSegment(
  px: number,
  py: number,
  ax: number,
  ay: number,
  bx: number,
  by: number,
): number {
  const abx = bx - ax;
  const aby = by - ay;
  const apx = px - ax;
  const apy = py - ay;
  const ab2 = abx * abx + aby * aby;
  if (ab2 < 1e-9) return Math.sqrt(dist2(px, py, ax, ay));
  let t = (apx * abx + apy * aby) / ab2;
  t = Math.max(0, Math.min(1, t));
  return Math.sqrt(dist2(px, py, ax + t * abx, ay + t * aby));
}

function flagColor(node: LaidOutNode, theme: CodeGraphTheme): string | null {
  const flags = node.flags ?? [];
  if (flags.some((f) => f.flag === "dead")) return theme.dead;
  if (flags.some((f) => f.flag === "unwired")) return theme.unwired;
  if (flags.some((f) => f.flag === "entry") || node.entry) return theme.entry;
  return null;
}

/** Match the app's canonical language swatches, including theme contrast. */
export function getCodeGraphColor(node: GraphVizNode, light = false): string {
  return getLanguageIconColor(nodeLanguageKey(node), light ? "light" : "dark");
}

export function paintCodeGraph(
  ctx: CanvasRenderingContext2D,
  req: CodeGraphPaintRequest,
): void {
  const { model, widthCss, heightCss, theme } = req;
  const globe = req.globe ?? null;
  const proj = globe?.projection ?? null;
  // Globe positions are already screen pixels: identity pan/scale.
  const panX = globe ? 0 : req.panX, panY = globe ? 0 : req.panY, scale = globe ? 1 : req.scale;
  const left = -panX / scale, right = (widthCss - panX) / scale;
  const top = -panY / scale, bottom = (heightCss - panY) / scale;
  const outside = (minX: number, minY: number, maxX: number, maxY: number) =>
    maxX < left || minX > right || maxY < top || minY > bottom;
  const nx = (i: number) => proj ? proj[i].x : model.nodes[i].x;
  const ny = (i: number) => proj ? proj[i].y : model.nodes[i].y;
  const isFront = (i: number) => !proj || proj[i].front;
  const nr = (i: number) => proj ? globeRadius(model.nodes[i], proj[i]) : model.nodes[i].radius;
  ctx.save();
  if (theme.background) {
    ctx.fillStyle = theme.background;
    ctx.fillRect(0, 0, widthCss, heightCss);
  } else ctx.clearRect(0, 0, widthCss, heightCss);
  ctx.translate(panX, panY);
  ctx.scale(scale, scale);

  const focus = req.selectedId || req.hoveredId;
  const neighbors = new Set<string>();
  if (focus && !req.traceIds) {
    neighbors.add(focus);
    for (const link of model.links) {
      if (link.source === focus) neighbors.add(link.target);
      if (link.target === focus) neighbors.add(link.source);
    }
  }
  const emphasized = (node: LaidOutNode) => req.traceIds ? req.traceIds.has(node.id) : focus ? neighbors.has(node.id) :
    (!req.activeCommunity || nodeCommunity(node) === req.activeCommunity) &&
    (!req.matchingIds || req.matchingIds.has(node.id));
  const occupied: Array<{ x: number; y: number; w: number; h: number }> = [];
  const label = (text: string, x: number, y: number, priority = false) => {
    const short = text.length > 32 ? text.slice(0, 29) + "…" : text;
    const w = short.length * 6.7 / scale + 10 / scale, h = 18 / scale;
    const box = { x: x - w / 2, y, w, h };
    if (!priority && occupied.some(b => box.x < b.x + b.w && box.x + w > b.x && y < b.y + b.h && y + h > b.y)) return;
    occupied.push(box);
    ctx.fillStyle = theme.background || theme.plate;
    ctx.fillRect(box.x, y, w, h);
    ctx.fillStyle = theme.label;
    ctx.font = `${priority ? "600 " : ""}${11 / scale}px ui-monospace, monospace`;
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    ctx.fillText(short, x, y + 3 / scale);
  };

  const visibleCommunities = new Map<string, number>();
  if (req.visibleIds) for (const node of model.nodes) {
    if (req.visibleIds.has(node.id)) {
      const name = nodeCommunity(node);
      visibleCommunities.set(name, (visibleCommunities.get(name) ?? 0) + 1);
    }
  }

  if (globe) {
    // The limb: a faint outline so the sphere reads as a sphere.
    ctx.globalAlpha = 0.35;
    ctx.beginPath();
    ctx.arc(globe.cx, globe.cy, globe.radius, 0, Math.PI * 2);
    ctx.strokeStyle = theme.field;
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.globalAlpha = 1;
  } else {
    // Subtle group fields keep disconnected files understandable without
    // inventing edges. Captions use paths while the legend retains group IDs.
    for (const group of model.communities) {
      if (!group.bounds || group.shown < 2) continue;
      const visibleCount = req.visibleIds ? visibleCommunities.get(group.name) ?? 0 : group.shown;
      if (!visibleCount) continue;
      const { x, y, radius } = group.bounds;
      const pad = 30 / scale;
      if (outside(x - radius - pad, y - radius - pad, x + radius + pad, y + radius + pad)) continue;
      const dim = req.activeCommunity && req.activeCommunity !== group.name ? 0.25 : 1;
      ctx.beginPath();
      ctx.arc(x, y, radius + 12 / scale, 0, Math.PI * 2);
      ctx.fillStyle = theme.field;
      ctx.globalAlpha = dim * 0.035;
      ctx.fill();
      ctx.strokeStyle = theme.field;
      ctx.globalAlpha = dim * 0.14;
      ctx.lineWidth = 1 / scale;
      ctx.stroke();
      ctx.globalAlpha = 1;
      if (radius * scale > 28 && occupied.length < 40 && y - radius >= top && y - radius <= bottom) label(`${group.label} · ${visibleCount}`, x, y - radius - 26 / scale);
    }
  }
  ctx.globalAlpha = 1;

  const paintNode = (i: number) => {
    const node = model.nodes[i];
    if (req.visibleIds && !req.visibleIds.has(node.id)) return;
    const x = nx(i), y = ny(i), r = nr(i);
    const pad = r + 6 / scale;
    if (outside(x - pad, y - pad, x + pad, y + pad)) return;
    const selected = req.selectedId === node.id, hovered = req.hoveredId === node.id;
    const color = getCodeGraphColor(node, req.light);
    const front = isFront(i);
    const lit = emphasized(node);
    ctx.globalAlpha = front ? (lit ? 1 : 0.16) : (lit ? GLOBE_BACK_ALPHA : GLOBE_BACK_ALPHA / 2);
    if (selected || hovered) {
      ctx.beginPath();
      ctx.arc(x, y, r + 5 / scale, 0, Math.PI * 2);
      ctx.fillStyle = color + "30";
      ctx.fill();
    }
    ctx.beginPath();
    ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fillStyle = color;
    ctx.fill();
    const flag = flagColor(node, theme);
    if (selected || hovered || flag) {
      ctx.strokeStyle = selected || hovered ? theme.selection : flag || color;
      ctx.lineWidth = (selected || hovered ? 2 : 1.5) / scale;
      ctx.stroke();
    }
  };

  const paintLinks = () => {
    const overviewAlpha = Math.min(0.35, Math.max(0.018, 0.7 / (1 + model.links.length / Math.max(1, model.nodes.length))));
    for (const [linkIndex, link] of model.links.entries()) {
      const a = model.nodes[link.sourceIndex], b = model.nodes[link.targetIndex];
      if (!a || !b) continue;
      if (req.visibleIds && (!req.visibleIds.has(a.id) || !req.visibleIds.has(b.id))) continue;
      const ax = nx(link.sourceIndex), ay = ny(link.sourceIndex), bx = nx(link.targetIndex), by = ny(link.targetIndex);
      const pad = 8 / scale;
      if (outside(Math.min(ax,bx)-pad, Math.min(ay,by)-pad, Math.max(ax,bx)+pad, Math.max(ay,by)+pad)) continue;
      const active = req.traceEdges ? req.traceEdges.has(linkIndex) : focus === a.id || focus === b.id;
      const bothFront = isFront(link.sourceIndex) && isFront(link.targetIndex);
      // Quiet overview edges only exist on the visible hemisphere; a traced or
      // selected edge that wraps round the back stays, faintly.
      if (globe && !bothFront && !active) continue;
      const visible = emphasized(a) && emphasized(b);
      // Dense overviews need quiet edges; selecting a node reveals its full
      // neighborhood. All relationships remain available for inspection.
      ctx.globalAlpha = (active ? 0.9 : (visible ? overviewAlpha : 0.025)) * (bothFront ? 1 : 0.3);
      ctx.beginPath();
      ctx.moveTo(ax, ay);
      ctx.lineTo(bx, by);
      ctx.strokeStyle = active ? getCodeGraphColor(a, req.light) : theme.linkMuted;
      ctx.lineWidth = (active ? 1.7 : 0.7) / scale;
      ctx.stroke();
      if (active && !isUndirectedGraphLink(link)) {
        const angle = Math.atan2(by - ay, bx - ax);
        const tipX = bx - Math.cos(angle) * (nr(link.targetIndex) + 2 / scale);
        const tipY = by - Math.sin(angle) * (nr(link.targetIndex) + 2 / scale);
        ctx.beginPath();
        ctx.moveTo(tipX - Math.cos(angle - 0.5) * 6 / scale, tipY - Math.sin(angle - 0.5) * 6 / scale);
        ctx.lineTo(tipX, tipY);
        ctx.lineTo(tipX - Math.cos(angle + 0.5) * 6 / scale, tipY - Math.sin(angle + 0.5) * 6 / scale);
        ctx.stroke();
      }
    }
  };

  if (globe) {
    // Back to front: the far hemisphere first, its edges and the near
    // hemisphere over it.
    for (let i = 0; i < model.nodes.length; i++) if (!isFront(i)) paintNode(i);
    paintLinks();
    for (let i = 0; i < model.nodes.length; i++) if (isFront(i)) paintNode(i);
  } else {
    paintLinks();
    for (let i = 0; i < model.nodes.length; i++) paintNode(i);
  }
  ctx.globalAlpha = 1;

  if (globe && proj) {
    // Cap captions replace the flat community outlines: one per group whose
    // members mostly face the viewer, at their visible centre.
    const caps = new Map<string, { n: number; front: number; x: number; y: number }>();
    for (let i = 0; i < model.nodes.length; i++) {
      const node = model.nodes[i];
      if (req.visibleIds && !req.visibleIds.has(node.id)) continue;
      const key = nodeCommunity(node);
      const cap = caps.get(key) ?? { n: 0, front: 0, x: 0, y: 0 };
      cap.n++;
      if (proj[i].depth > GLOBE_LABEL_MIN_DEPTH) { cap.front++; cap.x += proj[i].x; cap.y += proj[i].y; }
      caps.set(key, cap);
    }
    const ordered = model.communities.filter(g => caps.has(g.name)).sort((a, b) => (caps.get(b.name)?.n ?? 0) - (caps.get(a.name)?.n ?? 0));
    for (const group of ordered.slice(0, 24)) {
      const cap = caps.get(group.name);
      if (!cap || cap.front === 0 || cap.front * 2 < cap.n) continue;
      if (req.activeCommunity && req.activeCommunity !== group.name) continue;
      label(`${group.label} · ${cap.n}`, cap.x / cap.front, cap.y / cap.front - 9);
    }
  }

  // Selected/hovered labels survive density suppression; ordinary labels are
  // collision-checked and bounded so zooming into a large repo stays readable.
  // Back-face and limb-hugging nodes are never labelled.
  const candidates: LaidOutNode[] = [];
  const index = new Map<string, number>();
  for (let i = 0; i < model.nodes.length; i++) {
    const n = model.nodes[i];
    if (req.visibleIds && !req.visibleIds.has(n.id)) continue;
    if (proj && proj[i].depth <= GLOBE_LABEL_MIN_DEPTH) continue;
    const x = nx(i) * scale + panX, y = ny(i) * scale + panY;
    if (x >= 0 && x <= widthCss && y >= 0 && y <= heightCss &&
      (n.id === focus || n.id === req.hoveredId ||
      (req.showLabels !== false && emphasized(n) && (Boolean(focus) || model.nodes.length <= 40 || scale >= 1.8 || n.radius >= 6)))) {
      candidates.push(n);
      index.set(n.id, i);
    }
  }
  candidates.sort((a, b) => Number(b.id === focus) - Number(a.id === focus) || b.radius - a.radius);
  for (const node of candidates.slice(0, 80)) {
    const i = index.get(node.id) ?? 0;
    label(node.label || node.name || node.id, nx(i), ny(i) + nr(i) + 5 / scale, node.id === focus || node.id === req.selectedId);
  }
  ctx.restore();
}

/**
 * Hit-test in CSS pixel space (same as the canvas element's client coords
 * relative to its top-left). Converts through pan/scale into content space.
 * In globe mode pass pan 0 / scale 1 and the frame: back-hemisphere nodes and
 * links are never hit.
 */
export function hitTestCodeGraph(
  model: CodeGraphModel,
  cssX: number,
  cssY: number,
  panX: number,
  panY: number,
  scale: number,
  visibleIds?: ReadonlySet<string> | null,
  globe?: CodeGraphGlobeFrame | null,
): CodeGraphHit | null {
  const proj = globe?.projection ?? null;
  // Globe positions are screen pixels already: pan and scale do not apply.
  if (globe) { panX = 0; panY = 0; scale = 1; }
  if (!Number.isFinite(scale) || scale <= 0) return null;
  const x = (cssX - panX) / scale;
  const y = (cssY - panY) / scale;

  // Pick the nearest center within a usable CSS-pixel target. Reverse draw
  // order breaks ties, but a neighbor's padding cannot steal a direct hit.
  let nearest: LaidOutNode | null = null;
  let nearestDistance = Infinity;
  for (let i = model.nodes.length - 1; i >= 0; i--) {
    const node = model.nodes[i];
    if (visibleIds && !visibleIds.has(node.id)) continue;
    if (proj && !proj[i].front) continue;
    const px = proj ? proj[i].x : node.x, py = proj ? proj[i].y : node.y;
    const radius = proj ? globeRadius(node, proj[i]) : node.radius;
    const r = Math.max(radius, 5 / scale) + 2 / scale;
    const distance = dist2(x, y, px, py);
    if (distance <= r * r && distance < nearestDistance) {
      nearest = node;
      nearestDistance = distance;
    }
  }
  if (nearest) return { kind: "node", id: nearest.id, node: nearest };

  // Links — thicker pick tolerance in content space.
  const linkTol = 4 / scale;
  for (let i = 0; i < model.links.length; i++) {
    const link = model.links[i];
    const a = model.nodes[link.sourceIndex];
    const b = model.nodes[link.targetIndex];
    if (!a || !b) continue;
    if (visibleIds && (!visibleIds.has(a.id) || !visibleIds.has(b.id))) continue;
    if (proj && !(proj[link.sourceIndex].front && proj[link.targetIndex].front)) continue;
    const ax = proj ? proj[link.sourceIndex].x : a.x, ay = proj ? proj[link.sourceIndex].y : a.y;
    const bx = proj ? proj[link.targetIndex].x : b.x, by = proj ? proj[link.targetIndex].y : b.y;
    if (distToSegment(x, y, ax, ay, bx, by) <= linkTol) {
      return {
        kind: "link",
        id: `${link.source}→${link.target}`,
        linkIndex: i,
      };
    }
  }

  return null;
}
