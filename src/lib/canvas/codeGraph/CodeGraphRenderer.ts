/**
 * Free-2D code-graph canvas renderer.
 *
 * Deliberately separate from the commit `GraphRenderer` (lane solver /
 * VisualCommitRow). Reuses gpuContext + frameScheduler infrastructure only.
 * Hit-test kinds are symbol/file oriented: `node` | `link`.
 */

import { getBranchColor } from "../Palette";
import type { CodeGraphModel, LaidOutNode } from "../../codeintel/graphPayload";

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

export function paintCodeGraph(
  ctx: CanvasRenderingContext2D,
  req: CodeGraphPaintRequest,
): void {
  const { model, widthCss, heightCss, panX, panY, scale, theme } = req;
  ctx.save();
  if (theme.background) {
    ctx.fillStyle = theme.background;
    ctx.fillRect(0, 0, widthCss, heightCss);
  } else {
    ctx.clearRect(0, 0, widthCss, heightCss);
  }

  ctx.translate(panX, panY);
  ctx.scale(scale, scale);

  // Links
  for (const link of model.links) {
    const a = model.nodes[link.sourceIndex];
    const b = model.nodes[link.targetIndex];
    if (!a || !b) continue;
    const isHandoff = link.kind === "handoff";
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.strokeStyle = isHandoff ? theme.link : theme.linkMuted;
    ctx.lineWidth = isHandoff ? 1.5 / scale : 1 / scale;
    ctx.stroke();
  }

  // Nodes
  for (const node of model.nodes) {
    const override = flagColor(node);
    const fill = override ?? getBranchColor(node.colorIndex);
    const selected = req.selectedId === node.id;
    const hovered = req.hoveredId === node.id;

    ctx.beginPath();
    ctx.arc(node.x, node.y, node.radius, 0, Math.PI * 2);
    ctx.fillStyle = fill;
    ctx.fill();

    if (selected || hovered) {
      ctx.strokeStyle = selected ? theme.selection : theme.hover;
      ctx.lineWidth = (selected ? 2.5 : 1.75) / scale;
      ctx.stroke();
    }
  }

  if (req.showLabels !== false) {
    ctx.fillStyle = theme.label;
    ctx.font = `${11 / scale}px ui-sans-serif, system-ui, sans-serif`;
    ctx.textAlign = "center";
    ctx.textBaseline = "top";
    for (const node of model.nodes) {
      if (node.radius < 6 && model.nodes.length > 80) continue;
      const label = node.name || node.id;
      ctx.fillText(label, node.x, node.y + node.radius + 2 / scale);
    }
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
): CodeGraphHit | null {
  if (scale <= 0) return null;
  const x = (cssX - panX) / scale;
  const y = (cssY - panY) / scale;

  // Nodes first (top-most wins: reverse draw order).
  for (let i = model.nodes.length - 1; i >= 0; i--) {
    const node = model.nodes[i];
    const r = node.radius + 3;
    if (dist2(x, y, node.x, node.y) <= r * r) {
      return { kind: "node", id: node.id, node };
    }
  }

  // Links — thicker pick tolerance in content space.
  const linkTol = 4 / scale;
  for (let i = 0; i < model.links.length; i++) {
    const link = model.links[i];
    const a = model.nodes[link.sourceIndex];
    const b = model.nodes[link.targetIndex];
    if (!a || !b) continue;
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
