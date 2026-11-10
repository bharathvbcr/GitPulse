/**
 * Opt-in "globe" layout for the Files/Symbols graph canvases.
 *
 * Pure: no Svelte, store or DOM. Communities become caps on a unit sphere;
 * members spiral inside their cap; an orthographic projection turns the unit
 * points into screen coordinates for any yaw/pitch.
 *
 * Positions come back in the SAME order as `model.nodes`, so link
 * `sourceIndex`/`targetIndex` stay valid without remapping.
 */

import type { GraphVizNode } from "./types";

export interface SpherePoint {
  x: number;
  y: number;
  z: number;
}

/** The slice of `CodeGraphModel` the layout reads. */
export interface GlobeModelInput {
  nodes: ReadonlyArray<Pick<GraphVizNode, "id" | "community" | "area" | "degree" | "val">>;
}

export interface GlobeView {
  yaw: number;
  pitch: number;
  zoom: number;
  cx: number;
  cy: number;
  radius: number;
}

export interface ProjectedPoint {
  x: number;
  y: number;
  /** Rotated z: positive faces the viewer. */
  depth: number;
  front: boolean;
}

export const MAX_PITCH = (85 * Math.PI) / 180;
/** Share of the sphere's area the caps may cover before relaxation. */
const CAP_FILL = 0.8;
const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));
const MAX_RELAX_PASSES = 24;
/** Cap pair checks allowed per layout; larger graphs relax fewer (or zero) passes. */
const RELAX_PAIR_BUDGET = 6_000_000;

export function clampPitch(pitch: number): number {
  if (!Number.isFinite(pitch)) return 0;
  return Math.max(-MAX_PITCH, Math.min(MAX_PITCH, pitch));
}

/** Same rule as `nodeCommunity` in graphPayload; duplicated to keep this module dependency-free. */
function communityKey(node: GlobeModelInput["nodes"][number]): string {
  return (node.community || node.area || "").trim() || "_";
}

function compare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function normalize(p: SpherePoint): SpherePoint {
  const len = Math.hypot(p.x, p.y, p.z);
  if (len < 1e-12) return { x: 0, y: 0, z: 1 };
  return { x: p.x / len, y: p.y / len, z: p.z / len };
}

interface Cap {
  members: number[];
  theta: number;
  center: SpherePoint;
}

/**
 * Cap centres on a Fibonacci sphere, re-ordered so groups that are consecutive
 * in path order are neighbours: the lattice is cut into latitude bands and each
 * band is walked in alternating longitude direction (a snake).
 */
function snakeFibonacci(count: number): SpherePoint[] {
  const raw: Array<SpherePoint & { lon: number }> = [];
  for (let i = 0; i < count; i++) {
    const z = 1 - (2 * (i + 0.5)) / count;
    const r = Math.sqrt(Math.max(0, 1 - z * z));
    const lon = i * GOLDEN_ANGLE;
    raw.push({ x: r * Math.cos(lon), y: z, z: r * Math.sin(lon), lon: Math.atan2(r * Math.sin(lon), r * Math.cos(lon)) });
  }
  const bands = Math.max(1, Math.round(Math.sqrt(count)));
  const out: SpherePoint[] = [];
  for (let b = 0; b < bands; b++) {
    const lo = Math.floor((b * count) / bands);
    const hi = Math.floor(((b + 1) * count) / bands);
    const band = raw.slice(lo, hi).sort((p, q) => p.lon - q.lon);
    if (b % 2 === 1) band.reverse();
    for (const p of band) out.push({ x: p.x, y: p.y, z: p.z });
  }
  return out;
}

/** Push overlapping caps apart along the great circle between their centres. */
function relax(caps: Cap[]): void {
  const n = caps.length;
  if (n < 2) return;
  const pairs = (n * (n - 1)) / 2;
  const passes = Math.min(MAX_RELAX_PASSES, Math.floor(RELAX_PAIR_BUDGET / pairs));
  const dx = new Float64Array(n), dy = new Float64Array(n), dz = new Float64Array(n);
  for (let pass = 0; pass < passes; pass++) {
    dx.fill(0); dy.fill(0); dz.fill(0);
    let moved = false;
    for (let i = 0; i < n; i++) {
      const a = caps[i].center;
      for (let j = i + 1; j < n; j++) {
        const b = caps[j].center;
        const want = (caps[i].theta + caps[j].theta) * 0.85;
        if (want >= Math.PI) continue;
        const dot = Math.max(-1, Math.min(1, a.x * b.x + a.y * b.y + a.z * b.z));
        const gap = Math.acos(dot);
        if (gap >= want) continue;
        // Tangent at a pointing away from b (and vice versa); skip exact antipodes/coincidence.
        const tax = a.x * dot - b.x, tay = a.y * dot - b.y, taz = a.z * dot - b.z;
        const tbx = b.x * dot - a.x, tby = b.y * dot - a.y, tbz = b.z * dot - a.z;
        const la = Math.hypot(tax, tay, taz), lb = Math.hypot(tbx, tby, tbz);
        if (la < 1e-9 || lb < 1e-9) continue;
        const push = (want - gap) * 0.5;
        dx[i] += (tax / la) * push; dy[i] += (tay / la) * push; dz[i] += (taz / la) * push;
        dx[j] += (tbx / lb) * push; dy[j] += (tby / lb) * push; dz[j] += (tbz / lb) * push;
        moved = true;
      }
    }
    if (!moved) return;
    for (let i = 0; i < n; i++) {
      const c = caps[i].center;
      caps[i].center = normalize({ x: c.x + dx[i], y: c.y + dy[i], z: c.z + dz[i] });
    }
  }
}

/** Unit vector perpendicular to `c`, chosen from the axis least aligned with it. */
function perpendicular(c: SpherePoint): SpherePoint {
  const ax = Math.abs(c.x), ay = Math.abs(c.y), az = Math.abs(c.z);
  const axis: SpherePoint = ax <= ay && ax <= az ? { x: 1, y: 0, z: 0 } : ay <= az ? { x: 0, y: 1, z: 0 } : { x: 0, y: 0, z: 1 };
  return normalize({
    x: axis.y * c.z - axis.z * c.y,
    y: axis.z * c.x - axis.x * c.z,
    z: axis.x * c.y - axis.y * c.x,
  });
}

export function layoutOnSphere(model: GlobeModelInput): SpherePoint[] {
  const nodes = model.nodes;
  const total = nodes.length;
  if (total === 0) return [];

  const byKey = new Map<string, number[]>();
  nodes.forEach((node, index) => {
    const key = communityKey(node);
    const list = byKey.get(key);
    if (list) list.push(index); else byKey.set(key, [index]);
  });
  const keys = [...byKey.keys()].sort(compare);
  const centers = snakeFibonacci(keys.length);
  const caps: Cap[] = keys.map((key, i) => {
    const members = byKey.get(key) as number[];
    const cosTheta = Math.max(-1, 1 - (2 * CAP_FILL * members.length) / total);
    return { members, theta: Math.acos(cosTheta), center: centers[i] };
  });
  relax(caps);

  const out: SpherePoint[] = new Array<SpherePoint>(total);
  for (const cap of caps) {
    const n = cap.members.length;
    // Hubs sit at the cap's centre, as the flat layout does; ids break ties.
    const ordered = [...cap.members].sort((a, b) => {
      const da = nodes[a].degree ?? nodes[a].val ?? 0, db = nodes[b].degree ?? nodes[b].val ?? 0;
      return db - da || compare(nodes[a].id, nodes[b].id) || a - b;
    });
    const c = cap.center;
    const u = perpendicular(c);
    const v: SpherePoint = { x: c.y * u.z - c.z * u.y, y: c.z * u.x - c.x * u.z, z: c.x * u.y - c.y * u.x };
    ordered.forEach((nodeIndex, k) => {
      const phi = n === 1 ? 0 : cap.theta * Math.sqrt(k / (n - 1 + 0.5));
      const alpha = k * GOLDEN_ANGLE;
      const sp = Math.sin(phi), cp = Math.cos(phi);
      const ca = Math.cos(alpha), sa = Math.sin(alpha);
      out[nodeIndex] = normalize({
        x: c.x * cp + sp * (u.x * ca + v.x * sa),
        y: c.y * cp + sp * (u.y * ca + v.y * sa),
        z: c.z * cp + sp * (u.z * ca + v.z * sa),
      });
    });
  }
  return out;
}

/**
 * Orthographic projection: yaw about the vertical axis, then pitch about the
 * horizontal one. Screen +x is right and +y is down; positive depth faces the
 * viewer. Pass `out` to reuse one scratch array (and its objects) across frames.
 */
export function projectOrthographic(
  points: readonly SpherePoint[],
  view: GlobeView,
  out: ProjectedPoint[] = [],
): ProjectedPoint[] {
  const pitch = clampPitch(view.pitch);
  const cy_ = Math.cos(view.yaw), sy = Math.sin(view.yaw);
  const cp = Math.cos(pitch), sp = Math.sin(pitch);
  const scale = view.radius * view.zoom;
  for (let i = 0; i < points.length; i++) {
    const p = points[i];
    const x1 = p.x * cy_ + p.z * sy;
    const z1 = -p.x * sy + p.z * cy_;
    const y2 = p.y * cp - z1 * sp;
    const z2 = p.y * sp + z1 * cp;
    const slot = out[i];
    if (slot) {
      slot.x = view.cx + x1 * scale;
      slot.y = view.cy - y2 * scale;
      slot.depth = z2;
      slot.front = z2 > 0;
    } else {
      out[i] = { x: view.cx + x1 * scale, y: view.cy - y2 * scale, depth: z2, front: z2 > 0 };
    }
  }
  out.length = points.length;
  return out;
}

/** The yaw/pitch that bring `point` to the front-centre of the globe. */
export function rotationToFace(point: SpherePoint): { yaw: number; pitch: number } {
  const horizontal = Math.hypot(point.x, point.z);
  const yaw = horizontal < 1e-12 ? 0 : Math.atan2(-point.x, point.z);
  return { yaw, pitch: clampPitch(Math.atan2(point.y, horizontal)) };
}

/**
 * Drag rotation: the surface follows the pointer. Dragging right turns the
 * front face right (yaw grows), dragging down tips it down (pitch grows).
 * `radius` is the on-screen sphere radius, so one radius of drag is one radian.
 */
export function rotateByDrag(
  rotation: { yaw: number; pitch: number },
  dx: number,
  dy: number,
  radius: number,
): { yaw: number; pitch: number } {
  const per = radius > 0 ? 1 / radius : 0;
  return { yaw: rotation.yaw + dx * per, pitch: clampPitch(rotation.pitch + dy * per) };
}

/** Interpolate between two rotations, taking the short way round in yaw. */
export function lerpRotation(
  from: { yaw: number; pitch: number },
  to: { yaw: number; pitch: number },
  t: number,
): { yaw: number; pitch: number } {
  const turn = Math.PI * 2;
  let delta = (to.yaw - from.yaw) % turn;
  if (delta > Math.PI) delta -= turn;
  else if (delta < -Math.PI) delta += turn;
  return { yaw: from.yaw + delta * t, pitch: from.pitch + (to.pitch - from.pitch) * t };
}
