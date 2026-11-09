import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BAKED_SPRINGS, type SpringName } from "../src/lib/motion/springs.generated";
import {
  MAX_ERROR,
  MAX_POINTS,
  OUTPUT_PATH,
  SPRING_SPECS,
  bakeSpring,
  evaluatePolyline,
  generate,
  run,
  settleSeconds,
  springPosition,
  toLinear,
} from "./bake-springs.mjs";

const script = fileURLToPath(new URL("./bake-springs.mjs", import.meta.url));
const NAMES = Object.keys(BAKED_SPRINGS) as SpringName[];

type Stop = readonly [number, number];

/** Parse a `linear(...)` value the way a browser reads it: first stop at 0%, last at 100%. */
function parseLinear(value: string): Stop[] {
  const match = /^linear\((.*)\)$/.exec(value);
  if (!match || match[1] === undefined) throw new Error(`not a linear() value: ${value}`);
  const parts = match[1].split(",").map((part) => part.trim());
  return parts.map((part, i): Stop => {
    const [y, percent] = part.split(/\s+/);
    if (y === undefined) throw new Error(`empty stop in ${value}`);
    if (percent === undefined) {
      if (i !== 0 && i !== parts.length - 1) throw new Error(`interior stop without a position: ${part}`);
      return [i === 0 ? 0 : 1, Number(y)];
    }
    if (!percent.endsWith("%")) throw new Error(`bad position: ${part}`);
    return [Number(percent.slice(0, -1)) / 100, Number(y)];
  });
}

/**
 * An independent oracle: integrate x'' = k(1 - x) - c x' with RK4, so the
 * closed forms in the generator are not graded by themselves.
 */
function integrate(damping: number, stiffness: number, seconds: number): (t: number) => number {
  const c = 2 * damping * Math.sqrt(stiffness);
  const dt = 0.0001;
  const accel = (x: number, v: number) => stiffness * (1 - x) - c * v;
  const xs: number[] = [0];
  let x = 0;
  let v = 0;
  for (let step = 0; step * dt < seconds + dt; step += 1) {
    const k1x = v;
    const k1v = accel(x, v);
    const k2x = v + (dt / 2) * k1v;
    const k2v = accel(x + (dt / 2) * k1x, v + (dt / 2) * k1v);
    const k3x = v + (dt / 2) * k2v;
    const k3v = accel(x + (dt / 2) * k2x, v + (dt / 2) * k2v);
    const k4x = v + dt * k3v;
    const k4v = accel(x + dt * k3x, v + dt * k3v);
    x += (dt / 6) * (k1x + 2 * k2x + 2 * k3x + k4x);
    v += (dt / 6) * (k1v + 2 * k2v + 2 * k3v + k4v);
    xs.push(x);
  }
  return (t) => {
    const at = t / dt;
    const lo = Math.min(Math.floor(at), xs.length - 2);
    const a = xs[lo] ?? 0;
    const b = xs[lo + 1] ?? a;
    return a + (b - a) * (at - lo);
  };
}

const scratch: string[] = [];
afterEach(() => {
  vi.restoreAllMocks();
  while (scratch.length > 0) {
    const dir = scratch.pop();
    if (dir) rmSync(dir, { recursive: true, force: true });
  }
});

describe("the closed-form spring", () => {
  it.each(SPRING_SPECS.map((s) => [s.name, s.damping, s.stiffness] as const))(
    "%s matches an RK4 integration of the same oscillator",
    (_name, damping, stiffness) => {
      const reference = integrate(damping, stiffness, 1);
      for (let i = 0; i <= 100; i += 1) {
        const t = i / 100;
        expect(Math.abs(springPosition(damping, stiffness, t) - reference(t))).toBeLessThan(1e-4);
      }
    },
  );

  it("covers the overdamped branch continuously with the critically damped one", () => {
    const critical = springPosition(1, 900, 0.05);
    const nearly = springPosition(1.0001, 900, 0.05);
    expect(Math.abs(critical - nearly)).toBeLessThan(1e-3);
    expect(settleSeconds(1.5, 900)).toBeGreaterThan(0);
  });
});

describe("baked curves", () => {
  it("bakes exactly the five documented springs", () => {
    expect(NAMES.sort()).toEqual(["effect", "spatial", "spatialFast", "spatialSlow", "standard"]);
    const byName = new Map(SPRING_SPECS.map((s) => [s.name, s]));
    expect(byName.get("spatialFast")).toMatchObject({ damping: 0.6, stiffness: 800 });
    expect(byName.get("spatial")).toMatchObject({ damping: 0.8, stiffness: 380 });
    expect(byName.get("spatialSlow")).toMatchObject({ damping: 0.8, stiffness: 200 });
    expect(byName.get("effect")).toMatchObject({ damping: 1, stiffness: 1600 });
    expect(byName.get("standard")).toMatchObject({ damping: 0.9, stiffness: 700 });
  });

  describe.each(NAMES)("%s", (name) => {
    const baked = BAKED_SPRINGS[name];
    const stops = parseLinear(baked.linear);

    it("is what the generator would bake now (not hand-edited)", () => {
      const fresh = bakeSpring(baked.damping, baked.stiffness);
      expect(baked.linear).toBe(fresh.linear);
      expect(baked.durationMs).toBe(fresh.durationMs);
      expect(baked.points).toEqual(fresh.points);
      expect(toLinear(baked.points)).toBe(baked.linear);
    });

    it("starts at 0, ends at 1 and keeps stops strictly increasing", () => {
      expect(stops[0]).toEqual([0, 0]);
      expect(stops[stops.length - 1]).toEqual([1, 1]);
      for (let i = 1; i < stops.length; i += 1) {
        expect((stops[i] as Stop)[0]).toBeGreaterThan((stops[i - 1] as Stop)[0]);
      }
      // The string is the CSS truth; `points` is the same data as numbers.
      expect(stops).toHaveLength(baked.points.length);
      stops.forEach((stop, i) => {
        const point = baked.points[i] as Stop;
        expect(stop[0]).toBeCloseTo(point[0], 9);
        expect(stop[1]).toBe(point[1]);
      });
    });

    it(`uses at most ${MAX_POINTS} stops`, () => {
      expect(stops.length).toBeLessThanOrEqual(MAX_POINTS);
    });

    it(`is within ${MAX_ERROR} of the analytic spring at 200 sample points`, () => {
      const seconds = baked.durationMs / 1000;
      for (let i = 0; i < 200; i += 1) {
        const progress = i / 199;
        const truth = springPosition(baked.damping, baked.stiffness, progress * seconds);
        expect(Math.abs(evaluatePolyline(stops, progress) - truth)).toBeLessThanOrEqual(MAX_ERROR);
      }
    });

    it("is within the bound of an independent RK4 integration too", () => {
      const seconds = baked.durationMs / 1000;
      const reference = integrate(baked.damping, baked.stiffness, seconds);
      for (let i = 0; i < 200; i += 1) {
        const progress = i / 199;
        expect(Math.abs(evaluatePolyline(stops, progress) - reference(progress * seconds))).toBeLessThanOrEqual(MAX_ERROR);
      }
    });

    it("reports a settle time past which the spring is within 0.001 of 1", () => {
      const seconds = baked.durationMs / 1000;
      expect(seconds).toBeGreaterThan(settleSeconds(baked.damping, baked.stiffness) - 0.001);
      for (let t = seconds; t < seconds + 0.5; t += 0.005) {
        expect(Math.abs(springPosition(baked.damping, baked.stiffness, t) - 1)).toBeLessThan(0.001);
      }
    });
  });

  it("overshoots 1 for every underdamped spring", () => {
    for (const name of ["spatialFast", "spatial", "spatialSlow", "standard"] as const) {
      const { damping, stiffness, durationMs } = BAKED_SPRINGS[name];
      let peak = 0;
      for (let t = 0; t <= durationMs / 1000; t += 0.001) peak = Math.max(peak, springPosition(damping, stiffness, t));
      expect(peak, `${name} (analytic)`).toBeGreaterThan(1);
    }
  });

  it("keeps the overshoot in the polyline wherever it exceeds the error budget", () => {
    // zeta 0.9 overshoots by only ~0.15%, far under the 0.01 budget, so the
    // baked `standard` curve legitimately flattens it; the others must not.
    for (const name of ["spatialFast", "spatial", "spatialSlow"] as const) {
      const peak = Math.max(...parseLinear(BAKED_SPRINGS[name].linear).map((s) => s[1]));
      expect(peak, name).toBeGreaterThan(1);
    }
  });

  it("never exceeds 1 for the critically damped effect spring, between stops either", () => {
    const stops = parseLinear(BAKED_SPRINGS.effect.linear);
    expect(Math.max(...stops.map((s) => s[1]))).toBeLessThanOrEqual(1);
    for (let i = 0; i <= 1000; i += 1) {
      expect(evaluatePolyline(stops, i / 1000)).toBeLessThanOrEqual(1);
    }
    let previous = 0;
    for (const [, y] of stops) {
      expect(y).toBeGreaterThanOrEqual(previous);
      previous = y;
    }
  });

  it("bakes deterministically", () => {
    expect(generate()).toBe(generate());
  });

  it("refuses a spring that cannot settle", () => {
    expect(() => bakeSpring(0, 400)).toThrow(/does not settle/);
  });
});

describe("bake-springs --check", () => {
  it("passes on the committed file", () => {
    expect(readFileSync(OUTPUT_PATH, "utf8")).toBe(generate());
    const log = vi.spyOn(console, "log").mockImplementation(() => undefined);
    expect(run(["--check"])).toBe(0);
    expect(log).toHaveBeenCalledWith(expect.stringContaining("up to date"));
  });

  it("exits 0 from the real command line", () => {
    const out = execFileSync(process.execPath, [script, "--check"], { encoding: "utf8" });
    expect(out).toContain("up to date");
  });

  it("fails on a stale file and on a missing one, and the rewrite repairs it", () => {
    const dir = mkdtempSync(path.join(tmpdir(), "bake-springs-"));
    scratch.push(dir);
    const target = path.join(dir, "springs.generated.ts");
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    vi.spyOn(console, "log").mockImplementation(() => undefined);

    expect(run(["--check"], target)).toBe(1);
    writeFileSync(target, generate().replace("durationMs: ", "durationMs: 1"));
    expect(run(["--check"], target)).toBe(1);
    expect(run([], target)).toBe(0);
    expect(readFileSync(target, "utf8")).toBe(generate());
    expect(run(["--check"], target)).toBe(0);
  });

  it("rejects an unknown flag with exit 2 and answers --help with exit 0", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    vi.spyOn(console, "log").mockImplementation(() => undefined);
    expect(run(["--bogus"])).toBe(2);
    expect(run(["--help"])).toBe(0);
    const cli = spawnSync(process.execPath, [script, "--bogus"], { encoding: "utf8" });
    expect(cli.status).toBe(2);
  });
});
