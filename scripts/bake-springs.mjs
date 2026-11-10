#!/usr/bin/env node
/**
 * Bake spring parameters into CSS `linear()` easings.
 *
 * Writes `src/lib/motion/springs.generated.ts`. The curves are generated, not
 * typed: a hand-typed polyline drifts from the spring it claims to be, and
 * nothing would notice. `--check` is the guard (the committed file must equal
 * what this script would write now).
 *
 * Model: a unit mass on a spring of stiffness `k` with damping ratio `zeta`,
 * released from rest at 0 and pulled toward 1 (the step response).
 * `omega0 = sqrt(k)`, damping coefficient `c = 2 * zeta * sqrt(k)`. Time is in
 * seconds. The closed forms below cover under-, critically and over-damped.
 *
 * Settling: the duration is the last instant at which the curve is still
 * further than `SETTLE_POSITION` from 1 or still moving faster than
 * `SETTLE_VELOCITY` (units per second). The emitted curve ends exactly at 1.
 *
 * Sampling: Douglas-Peucker over a dense reference sampling of the analytic
 * curve, so stops land where the curve bends (the overshoot peaks) and
 * long flat stretches cost nothing. `linear()` stops carry explicit
 * percentages. The error budget is measured on the ROUNDED polyline that is
 * actually emitted, against the analytic curve, never on the pre-rounding one.
 *
 * Exit: 0 ok / written / up to date · 1 `--check` found a stale or missing
 * file · 2 usage error.
 */
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const OUTPUT_PATH = path.join(REPO, "src", "lib", "motion", "springs.generated.ts");

/** Settled when within this of 1 ... */
export const SETTLE_POSITION = 0.001;
/** ... and moving slower than this (units per second). */
export const SETTLE_VELOCITY = 0.01;
/** Acceptance bound for the emitted polyline against the analytic spring. */
export const MAX_ERROR = 0.01;
/** Upper bound on `linear()` stops per curve. */
export const MAX_POINTS = 40;

const REFERENCE_STEP_S = 0.00025;
const HORIZON_S = 10;
const REFERENCE_SAMPLES = 4000;

/**
 * @typedef {{ name: string, damping: number, stiffness: number, role: string }} SpringSpec
 * @typedef {readonly [number, number]} Point
 * @typedef {{ damping: number, stiffness: number, durationMs: number, points: Point[], linear: string, maxError: number }} BakedSpring
 */

/** @type {readonly SpringSpec[]} */
export const SPRING_SPECS = Object.freeze([
  { name: "spatialFast", damping: 0.6, stiffness: 800, role: "M3 Expressive fast spatial" },
  { name: "spatial", damping: 0.8, stiffness: 380, role: "M3 Expressive default spatial" },
  { name: "spatialSlow", damping: 0.8, stiffness: 200, role: "M3 Expressive slow spatial" },
  { name: "effect", damping: 1.0, stiffness: 1600, role: "M3 Expressive effects (never overshoots)" },
  { name: "standard", damping: 0.9, stiffness: 700, role: "M3 Standard spatial" },
]);

/**
 * Step response of the damped oscillator at time `t` seconds.
 * @param {number} damping zeta
 * @param {number} stiffness k (mass 1)
 * @param {number} t
 */
export function springPosition(damping, stiffness, t) {
  if (t <= 0) return 0;
  const w0 = Math.sqrt(stiffness);
  if (damping < 1) {
    const decay = damping * w0;
    const wd = w0 * Math.sqrt(1 - damping * damping);
    return 1 - Math.exp(-decay * t) * (Math.cos(wd * t) + (decay / wd) * Math.sin(wd * t));
  }
  if (damping === 1) return 1 - Math.exp(-w0 * t) * (1 + w0 * t);
  const root = w0 * Math.sqrt(damping * damping - 1);
  const slow = damping * w0 - root;
  const fast = damping * w0 + root;
  return 1 - (fast * Math.exp(-slow * t) - slow * Math.exp(-fast * t)) / (fast - slow);
}

/**
 * Velocity of the step response (units per second).
 * @param {number} damping
 * @param {number} stiffness
 * @param {number} t
 */
export function springVelocity(damping, stiffness, t) {
  if (t <= 0) return 0;
  const w0 = Math.sqrt(stiffness);
  if (damping < 1) {
    const decay = damping * w0;
    const wd = w0 * Math.sqrt(1 - damping * damping);
    return (w0 * w0 / wd) * Math.exp(-decay * t) * Math.sin(wd * t);
  }
  if (damping === 1) return w0 * w0 * t * Math.exp(-w0 * t);
  const root = w0 * Math.sqrt(damping * damping - 1);
  const slow = damping * w0 - root;
  const fast = damping * w0 + root;
  return ((fast * slow) / (fast - slow)) * (Math.exp(-slow * t) - Math.exp(-fast * t));
}

/**
 * Seconds until the spring has settled.
 * @param {number} damping
 * @param {number} stiffness
 */
export function settleSeconds(damping, stiffness) {
  let last = 0;
  for (let t = 0; t <= HORIZON_S; t += REFERENCE_STEP_S) {
    if (
      Math.abs(springPosition(damping, stiffness, t) - 1) >= SETTLE_POSITION ||
      Math.abs(springVelocity(damping, stiffness, t)) >= SETTLE_VELOCITY
    ) {
      last = t;
    }
  }
  if (last >= HORIZON_S - REFERENCE_STEP_S) {
    throw new Error(`spring (zeta ${damping}, k ${stiffness}) does not settle within ${HORIZON_S}s`);
  }
  return last + REFERENCE_STEP_S;
}

/**
 * Evaluate a polyline of (progress, value) stops at `x` in [0, 1], the way
 * CSS `linear()` does.
 * @param {readonly Point[]} points
 * @param {number} x
 */
export function evaluatePolyline(points, x) {
  const first = points[0];
  const last = points[points.length - 1];
  if (!first || !last) throw new Error("empty polyline");
  if (x <= first[0]) return first[1];
  if (x >= last[0]) return last[1];
  for (let i = 1; i < points.length; i += 1) {
    const b = /** @type {Point} */ (points[i]);
    if (x <= b[0]) {
      const a = /** @type {Point} */ (points[i - 1]);
      const span = b[0] - a[0];
      return span === 0 ? b[1] : a[1] + ((b[1] - a[1]) * (x - a[0])) / span;
    }
  }
  return last[1];
}

/**
 * Largest |polyline - spring| over a dense sampling of the whole duration.
 * @param {number} damping
 * @param {number} stiffness
 * @param {number} durationS
 * @param {readonly Point[]} points
 */
export function measureError(damping, stiffness, durationS, points) {
  let worst = 0;
  for (let i = 0; i <= REFERENCE_SAMPLES; i += 1) {
    const x = i / REFERENCE_SAMPLES;
    const truth = x === 1 ? 1 : springPosition(damping, stiffness, x * durationS);
    worst = Math.max(worst, Math.abs(evaluatePolyline(points, x) - truth));
  }
  return worst;
}

/**
 * Douglas-Peucker on a sampled curve; returns the kept sample indices.
 * @param {readonly Point[]} samples
 * @param {number} tolerance maximum vertical distance to the chord
 */
function simplify(samples, tolerance) {
  const keep = new Set([0, samples.length - 1]);
  /** @type {Array<[number, number]>} */
  const stack = [[0, samples.length - 1]];
  while (stack.length > 0) {
    const [lo, hi] = /** @type {[number, number]} */ (stack.pop());
    const a = /** @type {Point} */ (samples[lo]);
    const b = /** @type {Point} */ (samples[hi]);
    let worst = -1;
    let at = -1;
    for (let i = lo + 1; i < hi; i += 1) {
      const p = /** @type {Point} */ (samples[i]);
      const chord = a[1] + ((b[1] - a[1]) * (p[0] - a[0])) / (b[0] - a[0]);
      const gap = Math.abs(p[1] - chord);
      if (gap > worst) {
        worst = gap;
        at = i;
      }
    }
    if (at !== -1 && worst > tolerance) {
      keep.add(at);
      stack.push([lo, at], [at, hi]);
    }
  }
  return [...keep].sort((x, y) => x - y).map((i) => /** @type {Point} */ (samples[i]));
}

/** @param {number} n @param {number} places */
function round(n, places) {
  const f = 10 ** places;
  return Math.round(n * f) / f;
}

/** Shortest decimal text for an already-rounded number. @param {number} n */
function text(n) {
  return String(n);
}

/**
 * Round stops the way they are written, pin the ends to 0 and 1, and drop any
 * stop whose rounded position collides with its predecessor.
 * @param {readonly Point[]} points
 * @returns {Point[]}
 */
function roundStops(points) {
  /** @type {Point[]} */
  const out = [];
  points.forEach((p, i) => {
    const isFirst = i === 0;
    const isLast = i === points.length - 1;
    const x = isFirst ? 0 : isLast ? 1 : round(p[0], 4);
    const y = isFirst ? 0 : isLast ? 1 : round(p[1], 4);
    const prev = out[out.length - 1];
    if (prev && x <= prev[0]) {
      if (isLast) out[out.length - 1] = [x, y];
      return;
    }
    out.push([x, y]);
  });
  return out;
}

/**
 * Serialize stops as a CSS `linear()` value. The first stop is implicitly at
 * 0% and the last at 100%; the rest carry explicit percentages.
 * @param {readonly Point[]} points
 */
export function toLinear(points) {
  const parts = points.map((p, i) =>
    i === 0 || i === points.length - 1 ? text(p[1]) : `${text(p[1])} ${text(round(p[0] * 100, 2))}%`,
  );
  return `linear(${parts.join(", ")})`;
}

/**
 * Bake one spring. Tightens the simplification tolerance until the ROUNDED
 * polyline is within MAX_ERROR of the analytic curve; fails loudly when no
 * tolerance fits MAX_POINTS.
 * @param {number} damping
 * @param {number} stiffness
 * @returns {BakedSpring}
 */
export function bakeSpring(damping, stiffness) {
  const durationS = settleSeconds(damping, stiffness);
  /** @type {Point[]} */
  const dense = [];
  for (let i = 0; i <= REFERENCE_SAMPLES; i += 1) {
    const x = i / REFERENCE_SAMPLES;
    dense.push([x, springPosition(damping, stiffness, x * durationS)]);
  }
  for (let tolerance = 0.008; tolerance >= 0.0005; tolerance *= 0.9) {
    const stops = roundStops(simplify(dense, tolerance));
    const maxError = measureError(damping, stiffness, durationS, stops);
    if (maxError <= MAX_ERROR) {
      if (stops.length > MAX_POINTS) break;
      return {
        damping,
        stiffness,
        durationMs: Math.ceil(durationS * 1000),
        points: stops,
        linear: toLinear(stops),
        maxError: round(maxError, 5),
      };
    }
  }
  throw new Error(`spring (zeta ${damping}, k ${stiffness}) cannot be baked within ${MAX_POINTS} points at ${MAX_ERROR} error`);
}

/** The exact text of `springs.generated.ts`. */
export function generate() {
  const baked = SPRING_SPECS.map((spec) => ({ spec, spring: bakeSpring(spec.damping, spec.stiffness) }));
  const names = baked.map(({ spec }) => `"${spec.name}"`).join(" | ");
  const lines = [
    "// GENERATED FILE - do not edit by hand.",
    "// Source: scripts/bake-springs.mjs (damped harmonic oscillator, unit mass, step response).",
    "// Regenerate: node scripts/bake-springs.mjs",
    "// Verify:     node scripts/bake-springs.mjs --check",
    "",
    `export type SpringName = ${names};`,
    "",
    "export interface BakedSpring {",
    "  /** Damping ratio (zeta). */",
    "  readonly damping: number;",
    "  /** Stiffness k, mass 1. */",
    "  readonly stiffness: number;",
    "  /** Time for the spring to settle, in milliseconds (rounded up). */",
    "  readonly durationMs: number;",
    "  /** CSS `linear()` easing value. */",
    "  readonly linear: string;",
    "  /** The same stops as [progress, value] pairs; progress and value run 0..1. */",
    "  readonly points: readonly (readonly [number, number])[];",
    "  /** Largest |linear() - analytic spring| over the whole duration. */",
    "  readonly maxError: number;",
    "}",
    "",
    "export const BAKED_SPRINGS: Readonly<Record<SpringName, BakedSpring>> = {",
  ];
  for (const { spec, spring } of baked) {
    lines.push(
      `  // ${spec.role}: zeta ${spec.damping}, k ${spec.stiffness}`,
      `  ${spec.name}: {`,
      `    damping: ${spec.damping},`,
      `    stiffness: ${spec.stiffness},`,
      `    durationMs: ${spring.durationMs},`,
      `    linear: "${spring.linear}",`,
      `    points: [${spring.points.map((p) => `[${text(p[0])}, ${text(p[1])}]`).join(", ")}],`,
      `    maxError: ${spring.maxError},`,
      "  },",
    );
  }
  lines.push("};", "");
  return lines.join("\n");
}

function usage() {
  return formatUsage({
    name: "bake-springs",
    summary: "Bake M3 spring parameters into CSS linear() easings (src/lib/motion/springs.generated.ts).",
    flags: [
      { flag: "--check", description: "Exit 1 if the committed file differs from what would be generated" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 ok · 1 stale or missing file under --check · 2 usage error",
  });
}

/**
 * @param {string[]} argv
 * @param {string} [outputPath] where the generated file lives (tests redirect it)
 */
export function run(argv, outputPath = OUTPUT_PATH) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  const unknown = argv.filter((arg) => arg !== "--check");
  if (unknown.length > 0) {
    console.error(`unknown argument: ${unknown.join(" ")}\n\n${usage()}`);
    return 2;
  }
  const wanted = generate();
  if (argv.includes("--check")) {
    if (!existsSync(outputPath)) {
      console.error(`bake-springs: ${outputPath} is missing; run node scripts/bake-springs.mjs`);
      return 1;
    }
    if (readFileSync(outputPath, "utf8") !== wanted) {
      console.error(`bake-springs: ${outputPath} is stale; run node scripts/bake-springs.mjs`);
      return 1;
    }
    console.log("bake-springs: springs.generated.ts is up to date");
    return 0;
  }
  writeFileSync(outputPath, wanted);
  console.log(`bake-springs: wrote ${path.relative(REPO, outputPath)}`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = run(process.argv.slice(2));
}
