import { describe, expect, it } from "vitest";
import {
  MAX_PITCH, clampPitch, layoutOnSphere, projectOrthographic, rotationToFace,
  type GlobeModelInput, type SpherePoint,
} from "./globeLayout";

type N = GlobeModelInput["nodes"][number];

function model(groups: Record<string, number>): GlobeModelInput {
  const nodes: N[] = [];
  for (const [community, count] of Object.entries(groups)) {
    for (let i = 0; i < count; i++) nodes.push({ id: `${community}#${i}`, community, degree: (i * 7) % 5 });
  }
  return { nodes };
}

const chord = (a: SpherePoint, b: SpherePoint): number => Math.hypot(a.x - b.x, a.y - b.y, a.z - b.z);

function meanDistances(m: GlobeModelInput, pts: SpherePoint[]): { within: number; across: number } {
  let within = 0, wn = 0, across = 0, an = 0;
  for (let i = 0; i < pts.length; i++) {
    for (let j = i + 1; j < pts.length; j++) {
      const d = chord(pts[i], pts[j]);
      if (m.nodes[i].community === m.nodes[j].community) { within += d; wn++; } else { across += d; an++; }
    }
  }
  return { within: within / wn, across: across / an };
}

const sample = model({ "src/a": 40, "src/b": 40, "src/c": 30, "lib/x": 25, "lib/y": 25, "docs": 10 });

describe("layoutOnSphere", () => {
  it("returns unit-length, finite points in node order", () => {
    const pts = layoutOnSphere(sample);
    expect(pts).toHaveLength(sample.nodes.length);
    for (const p of pts) {
      expect(Number.isFinite(p.x + p.y + p.z)).toBe(true);
      expect(Math.hypot(p.x, p.y, p.z)).toBeCloseTo(1, 9);
    }
  });

  it("is deterministic and independent of an unrelated input copy", () => {
    const again = layoutOnSphere({ nodes: sample.nodes.map((n) => ({ ...n })) });
    expect(again).toEqual(layoutOnSphere(sample));
  });

  it("keeps node order: interleaved input still groups by community at the same index", () => {
    const nodes: N[] = Array.from({ length: 120 }, (_, i) => ({ id: `n${i}`, community: `c${i % 6}` }));
    const m = { nodes };
    const { within, across } = meanDistances(m, layoutOnSphere(m));
    expect(within).toBeLessThan(across * 0.7);
  });

  it("does not depend on input order for a node's community placement", () => {
    const reversed = { nodes: [...sample.nodes].reverse() };
    const a = layoutOnSphere(sample), b = layoutOnSphere(reversed).reverse();
    // Same community -> same cap: a node pair's within/across relation is preserved.
    const ma = meanDistances(sample, a), mb = meanDistances(sample, b);
    expect(mb.within).toBeCloseTo(ma.within, 6);
    expect(mb.across).toBeCloseTo(ma.across, 6);
  });

  it("puts same-community nodes closer than cross-community nodes on average", () => {
    const { within, across } = meanDistances(sample, layoutOnSphere(sample));
    expect(within).toBeLessThan(across * 0.6);
  });

  it("gives larger communities wider caps", () => {
    const m = model({ big: 200, small: 5, mid: 40 });
    const pts = layoutOnSphere(m);
    const spread = (name: string): number => {
      const idx = m.nodes.flatMap((n, i) => (n.community === name ? [i] : []));
      const c = { x: 0, y: 0, z: 0 };
      for (const i of idx) { c.x += pts[i].x; c.y += pts[i].y; c.z += pts[i].z; }
      const len = Math.hypot(c.x, c.y, c.z);
      return Math.max(...idx.map((i) => Math.acos(Math.min(1, (pts[i].x * c.x + pts[i].y * c.y + pts[i].z * c.z) / len))));
    };
    expect(spread("big")).toBeGreaterThan(spread("mid"));
    expect(spread("mid")).toBeGreaterThan(spread("small"));
  });

  it("separates caps: no two distinct nodes coincide", () => {
    const pts = layoutOnSphere(sample);
    let min = Infinity;
    for (let i = 0; i < pts.length; i++) for (let j = i + 1; j < pts.length; j++) min = Math.min(min, chord(pts[i], pts[j]));
    expect(min).toBeGreaterThan(1e-4);
  });

  it("falls back to area, then a shared bucket, when community is missing", () => {
    const pts = layoutOnSphere({ nodes: [
      { id: "a", area: "x" }, { id: "b", area: "x" }, { id: "c" }, { id: "d", community: "  " },
    ] });
    expect(pts).toHaveLength(4);
    expect(chord(pts[0], pts[1])).toBeLessThan(2);
    expect(chord(pts[2], pts[3])).toBeLessThan(chord(pts[0], pts[2]) + 2);
  });

  it("handles empty and single-node models", () => {
    expect(layoutOnSphere({ nodes: [] })).toEqual([]);
    const [p] = layoutOnSphere({ nodes: [{ id: "only", community: "c" }] });
    expect(Math.hypot(p.x, p.y, p.z)).toBeCloseTo(1, 9);
  });

  it("lays out 5000 nodes in sane time, in few and in many communities", () => {
    for (const groups of [10, 500, 5000]) {
      const nodes: N[] = Array.from({ length: 5000 }, (_, i) => ({ id: `n${i}`, community: `g${String(i % groups).padStart(4, "0")}` }));
      const started = performance.now();
      const pts = layoutOnSphere({ nodes });
      const elapsed = performance.now() - started;
      expect(pts).toHaveLength(5000);
      expect(Math.hypot(pts[4999].x, pts[4999].y, pts[4999].z)).toBeCloseTo(1, 9);
      expect(elapsed).toBeLessThan(3000);
    }
  });
});

describe("clampPitch", () => {
  it("clamps to +/-85 degrees and neutralises non-finite input", () => {
    expect(MAX_PITCH).toBeCloseTo((85 * Math.PI) / 180, 12);
    expect(clampPitch(Math.PI)).toBe(MAX_PITCH);
    expect(clampPitch(-Math.PI)).toBe(-MAX_PITCH);
    expect(clampPitch(0.3)).toBe(0.3);
    expect(clampPitch(Number.NaN)).toBe(0);
  });
});

describe("projectOrthographic", () => {
  const view = { yaw: 0, pitch: 0, zoom: 1, cx: 100, cy: 50, radius: 40 };

  it("projects known points and flags front/back", () => {
    const out = projectOrthographic([
      { x: 0, y: 0, z: 1 }, { x: 0, y: 0, z: -1 }, { x: 1, y: 0, z: 0 }, { x: 0, y: 1, z: 0 },
    ], view);
    expect(out[0]).toMatchObject({ x: 100, y: 50, front: true });
    expect(out[0].depth).toBeCloseTo(1, 12);
    expect(out[1]).toMatchObject({ x: 100, y: 50, front: false });
    expect(out[1].depth).toBeCloseTo(-1, 12);
    expect(out[2].x).toBeCloseTo(140, 9);
    expect(out[2].front).toBe(false); // exactly on the limb is not front
    expect(out[3].y).toBeCloseTo(10, 9); // up is screen-negative y
  });

  it("applies yaw: a quarter turn brings +x to the front", () => {
    const [p] = projectOrthographic([{ x: 1, y: 0, z: 0 }], { ...view, yaw: -Math.PI / 2 });
    expect(p.front).toBe(true);
    expect(p.depth).toBeCloseTo(1, 9);
    expect(p.x).toBeCloseTo(100, 9);
  });

  it("clamps pitch and scales by zoom", () => {
    const [p] = projectOrthographic([{ x: 0, y: 1, z: 0 }], { ...view, pitch: 10, zoom: 2 });
    expect(p.depth).toBeCloseTo(Math.sin(MAX_PITCH), 9);
    expect(p.y).toBeCloseTo(50 - Math.cos(MAX_PITCH) * 80, 9);
  });

  it("reuses a scratch buffer and its objects, resizing to the input", () => {
    const scratch = projectOrthographic([{ x: 0, y: 0, z: 1 }, { x: 0, y: 0, z: -1 }], view);
    const first = scratch[0];
    const again = projectOrthographic([{ x: 1, y: 0, z: 0 }], view, scratch);
    expect(again).toBe(scratch);
    expect(again).toHaveLength(1);
    expect(again[0]).toBe(first);
    expect(again[0].x).toBeCloseTo(140, 9);
  });

  it("keeps projected radius within the globe radius for unit points", () => {
    const pts = layoutOnSphere(sample);
    for (const p of projectOrthographic(pts, { ...view, yaw: 1.1, pitch: -0.4 })) {
      expect(Math.hypot(p.x - 100, p.y - 50)).toBeLessThanOrEqual(40 + 1e-9);
    }
  });
});

describe("rotationToFace", () => {
  it("round-trips: the target lands at front-centre", () => {
    const targets: SpherePoint[] = [
      { x: 0, y: 0, z: 1 }, { x: 1, y: 0, z: 0 }, { x: 0, y: 0, z: -1 },
      ...layoutOnSphere(sample).filter((_, i) => i % 17 === 0).filter((p) => Math.abs(p.y) < Math.sin(MAX_PITCH)),
    ];
    for (const t of targets) {
      const { yaw, pitch } = rotationToFace(t);
      const [p] = projectOrthographic([t], { yaw, pitch, zoom: 1, cx: 0, cy: 0, radius: 1 });
      expect(p.x).toBeCloseTo(0, 9);
      expect(p.y).toBeCloseTo(0, 9);
      expect(p.depth).toBeCloseTo(1, 9);
    }
  });

  it("clamps at the poles instead of exceeding the pitch limit", () => {
    const { pitch } = rotationToFace({ x: 0, y: 1, z: 0 });
    expect(pitch).toBe(MAX_PITCH);
    expect(rotationToFace({ x: 0, y: -1, z: 0 }).pitch).toBe(-MAX_PITCH);
  });
});
