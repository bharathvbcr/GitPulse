/**
 * Free-2D code-graph canvas renderer.
 *
 * Deliberately separate from the commit `GraphRenderer` (lane solver /
 * VisualCommitRow). Reuses gpuContext + frameScheduler infrastructure only.
 * Hit-test kinds are symbol/file oriented: `node` | `link`.
 */

import { nodeCommunity, nodeLanguageKey } from "../../codeintel/graphPayload";
import { isUndirectedGraphLink } from "../../codeintel/graphNavigation";
import type { CodeGraphModel, LaidOutNode } from "../../codeintel/graphPayload";
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

export interface CodeGraphTheme {
  background: string | null;
  link: string;
  linkMuted: string;
  label: string;
  selection: string;
  hover: string;
}

export const DEFAULT_CODE_GRAPH_THEME: CodeGraphTheme = {
  background: null,
  link: "rgba(120, 140, 170, 0.45)",
  linkMuted: "rgba(120, 140, 170, 0.22)",
  label: "rgba(180, 190, 210, 0.92)",
  selection: "#3d8bfd",
  hover: "#f0ad4e",
};

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

function flagColor(node: LaidOutNode): string | null {
  const flags = node.flags ?? [];
  if (flags.some((f) => f.flag === "dead")) return "#e35d6a";
  if (flags.some((f) => f.flag === "unwired")) return "#f0ad4e";
  if (flags.some((f) => f.flag === "entry") || node.entry) return "#34d399";
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
  const { model, widthCss, heightCss, panX, panY, scale, theme } = req;
  const left = -panX / scale, right = (widthCss - panX) / scale;
  const top = -panY / scale, bottom = (heightCss - panY) / scale;
  const outside = (minX: number, minY: number, maxX: number, maxY: number) =>
    maxX < left || minX > right || maxY < top || minY > bottom;
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
    ctx.fillStyle = theme.background || (req.light ? "#f8fafc" : "#171e2b");
    ctx.fillRect(box.x, y, w, h);
    ctx.fillStyle = theme.label;
    ctx.font = `${priority ? "600 " : ""}${11 / scale}px ui-monospace, monospace`;
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    ctx.fillText(short, x, y + 3 / scale);
  };

  // Subtle group fields keep disconnected files understandable without
  // inventing edges. Captions use paths while the legend retains group IDs.
  const visibleCommunities = new Map<string, number>();
  if (req.visibleIds) for (const node of model.nodes) {
    if (req.visibleIds.has(node.id)) {
      const name = nodeCommunity(node);
      visibleCommunities.set(name, (visibleCommunities.get(name) ?? 0) + 1);
    }
  }
  for (const group of model.communities) {
    if (!group.bounds || group.shown < 2) continue;
    const visibleCount = req.visibleIds ? visibleCommunities.get(group.name) ?? 0 : group.shown;
    if (!visibleCount) continue;
    const { x, y, radius } = group.bounds;
    const pad = 30 / scale;
    if (outside(x - radius - pad, y - radius - pad, x + radius + pad, y + radius + pad)) continue;
    const color = req.light ? "#64748b" : "#94a3b8";
    ctx.globalAlpha = req.activeCommunity && req.activeCommunity !== group.name ? 0.25 : 1;
    ctx.beginPath();
    ctx.arc(x, y, radius + 12 / scale, 0, Math.PI * 2);
    ctx.fillStyle = color + "09";
    ctx.fill();
    ctx.strokeStyle = color + "24";
    ctx.lineWidth = 1 / scale;
    ctx.stroke();
    if (radius * scale > 28 && occupied.length < 40 && y - radius >= top && y - radius <= bottom) label(`${group.label} · ${visibleCount}`, x, y - radius - 26 / scale);
  }
  ctx.globalAlpha = 1;

  const overviewAlpha = Math.min(0.35, Math.max(0.018, 0.7 / (1 + model.links.length / Math.max(1, model.nodes.length))));
  for (const [linkIndex, link] of model.links.entries()) {
    const a = model.nodes[link.sourceIndex], b = model.nodes[link.targetIndex];
    if (!a || !b) continue;
    if (req.visibleIds && (!req.visibleIds.has(a.id) || !req.visibleIds.has(b.id))) continue;
    const pad = 8 / scale;
    if (outside(Math.min(a.x,b.x)-pad, Math.min(a.y,b.y)-pad, Math.max(a.x,b.x)+pad, Math.max(a.y,b.y)+pad)) continue;
    const active = req.traceEdges ? req.traceEdges.has(linkIndex) : focus === a.id || focus === b.id;
    const visible = emphasized(a) && emphasized(b);
    // Dense overviews need quiet edges; selecting a node reveals its full
    // neighborhood. All relationships remain available for inspection.
    ctx.globalAlpha = active ? 0.9 : (visible ? overviewAlpha : 0.025);
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.strokeStyle = active ? getCodeGraphColor(a, req.light) : theme.linkMuted;
    ctx.lineWidth = (active ? 1.7 : 0.7) / scale;
    ctx.stroke();
    if (active && !isUndirectedGraphLink(link)) {
      const angle = Math.atan2(b.y - a.y, b.x - a.x);
      const tipX = b.x - Math.cos(angle) * (b.radius + 2 / scale);
      const tipY = b.y - Math.sin(angle) * (b.radius + 2 / scale);
      ctx.beginPath();
      ctx.moveTo(tipX - Math.cos(angle - 0.5) * 6 / scale, tipY - Math.sin(angle - 0.5) * 6 / scale);
      ctx.lineTo(tipX, tipY);
      ctx.lineTo(tipX - Math.cos(angle + 0.5) * 6 / scale, tipY - Math.sin(angle + 0.5) * 6 / scale);
      ctx.stroke();
    }
  }

  for (const node of model.nodes) {
    if (req.visibleIds && !req.visibleIds.has(node.id)) continue;
    const pad = node.radius + 6 / scale;
    if (outside(node.x-pad,node.y-pad,node.x+pad,node.y+pad)) continue;
    const selected = req.selectedId === node.id, hovered = req.hoveredId === node.id;
    const color = getCodeGraphColor(node, req.light);
    ctx.globalAlpha = emphasized(node) ? 1 : 0.16;
    if (selected || hovered) {
      ctx.beginPath();
      ctx.arc(node.x, node.y, node.radius + 5 / scale, 0, Math.PI * 2);
      ctx.fillStyle = color + "30";
      ctx.fill();
    }
    ctx.beginPath();
    ctx.arc(node.x, node.y, node.radius, 0, Math.PI * 2);
    ctx.fillStyle = color;
    ctx.fill();
    const flag = flagColor(node);
    if (selected || hovered || flag) {
      ctx.strokeStyle = selected || hovered ? theme.selection : flag || color;
      ctx.lineWidth = (selected || hovered ? 2 : 1.5) / scale;
      ctx.stroke();
    }
  }
  ctx.globalAlpha = 1;
  // Selected/hovered labels survive density suppression; ordinary labels are
  // collision-checked and bounded so zooming into a large repo stays readable.
  const candidates = model.nodes.filter(n => {
    if (req.visibleIds && !req.visibleIds.has(n.id)) return false;
    const x = n.x * scale + panX, y = n.y * scale + panY;
    return x >= 0 && x <= widthCss && y >= 0 && y <= heightCss &&
      (n.id === focus || n.id === req.hoveredId ||
      (req.showLabels !== false && emphasized(n) && (Boolean(focus) || model.nodes.length <= 40 || scale >= 1.8 || n.radius >= 6)));
  });
  candidates.sort((a, b) => Number(b.id === focus) - Number(a.id === focus) || b.radius - a.radius);
  for (const node of candidates.slice(0, 80)) {
    const x = node.x * scale + panX, y = node.y * scale + panY;
    if (x < 0 || x > widthCss || y < 0 || y > heightCss) continue;
    label(node.name || node.id, node.x, node.y + node.radius + 5 / scale, node.id === focus || node.id === req.selectedId);
  }
  ctx.restore();
}

/**
 * Hit-test in CSS pixel space (same as the canvas element's client coords
 * relative to its top-left). Converts through pan/scale into content space.
 */
export function hitTestCodeGraph(
  model: CodeGraphModel,
  cssX: number,
  cssY: number,
  panX: number,
  panY: number,
  scale: number,
  visibleIds?: ReadonlySet<string> | null,
): CodeGraphHit | null {
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
    const r = Math.max(node.radius, 5 / scale) + 2 / scale;
    const distance = dist2(x, y, node.x, node.y);
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
    if (distToSegment(x, y, a.x, a.y, b.x, b.y) <= linkTol) {
      return {
        kind: "link",
        id: `${link.source}→${link.target}`,
        linkIndex: i,
      };
    }
  }

  return null;
}
