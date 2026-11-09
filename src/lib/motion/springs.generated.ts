// GENERATED FILE - do not edit by hand.
// Source: scripts/bake-springs.mjs (damped harmonic oscillator, unit mass, step response).
// Regenerate: node scripts/bake-springs.mjs
// Verify:     node scripts/bake-springs.mjs --check

export type SpringName = "spatialFast" | "spatial" | "spatialSlow" | "effect" | "standard";

export interface BakedSpring {
  /** Damping ratio (zeta). */
  readonly damping: number;
  /** Stiffness k, mass 1. */
  readonly stiffness: number;
  /** Time for the spring to settle, in milliseconds (rounded up). */
  readonly durationMs: number;
  /** CSS `linear()` easing value. */
  readonly linear: string;
  /** The same stops as [progress, value] pairs; progress and value run 0..1. */
  readonly points: readonly (readonly [number, number])[];
  /** Largest |linear() - analytic spring| over the whole duration. */
  readonly maxError: number;
}

export const BAKED_SPRINGS: Readonly<Record<SpringName, BakedSpring>> = {
  // M3 Expressive fast spatial: zeta 0.6, k 800
  spatialFast: {
    damping: 0.6,
    stiffness: 800,
    durationMs: 482,
    linear: "linear(0, 0.0308 1.93%, 0.1243 4.13%, 0.247 6.2%, 0.6547 12.4%, 0.7691 14.43%, 0.8629 16.38%, 0.9401 18.35%, 1.001 20.37%, 1.0456 22.45%, 1.0756 24.65%, 1.0931 27.5%, 1.0919 30.78%, 1.0155 44.65%, 0.993 53.33%, 1)",
    points: [[0, 0], [0.0193, 0.0308], [0.0413, 0.1243], [0.062, 0.247], [0.124, 0.6547], [0.1443, 0.7691], [0.1638, 0.8629], [0.1835, 0.9401], [0.2037, 1.001], [0.2245, 1.0456], [0.2465, 1.0756], [0.275, 1.0931], [0.3078, 1.0919], [0.4465, 1.0155], [0.5333, 0.993], [1, 1]],
    maxError: 0.00723,
  },
  // M3 Expressive default spatial: zeta 0.8, k 380
  spatial: {
    damping: 0.8,
    stiffness: 380,
    durationMs: 484,
    linear: "linear(0, 0.0245 2.5%, 0.0994 5.43%, 0.5401 17.05%, 0.726 22.98%, 0.8567 28.88%, 0.9441 35.28%, 0.9926 42.23%, 1.0134 50.93%, 1)",
    points: [[0, 0], [0.025, 0.0245], [0.0543, 0.0994], [0.1705, 0.5401], [0.2298, 0.726], [0.2888, 0.8567], [0.3528, 0.9441], [0.4223, 0.9926], [0.5093, 1.0134], [1, 1]],
    maxError: 0.00745,
  },
  // M3 Expressive slow spatial: zeta 0.8, k 200
  spatialSlow: {
    damping: 0.8,
    stiffness: 200,
    durationMs: 653,
    linear: "linear(0, 0.0244 2.55%, 0.0995 5.55%, 0.539 17.4%, 0.7243 23.43%, 0.8546 29.4%, 0.9417 35.83%, 0.9916 42.95%, 1.0132 51.8%, 1)",
    points: [[0, 0], [0.0255, 0.0244], [0.0555, 0.0995], [0.174, 0.539], [0.2343, 0.7243], [0.294, 0.8546], [0.3583, 0.9417], [0.4295, 0.9916], [0.518, 1.0132], [1, 1]],
    maxError: 0.00761,
  },
  // M3 Expressive effects (never overshoots): zeta 1, k 1600
  effect: {
    damping: 1,
    stiffness: 1600,
    durationMs: 267,
    linear: "linear(0, 0.0198 2%, 0.0803 4.38%, 0.47 14.85%, 0.6521 20.88%, 0.7866 27.25%, 0.8808 34.38%, 0.9444 43.25%, 0.9801 54.73%, 0.9954 70.5%, 1)",
    points: [[0, 0], [0.02, 0.0198], [0.0438, 0.0803], [0.1485, 0.47], [0.2088, 0.6521], [0.2725, 0.7866], [0.3438, 0.8808], [0.4325, 0.9444], [0.5473, 0.9801], [0.705, 0.9954], [1, 1]],
    maxError: 0.00698,
  },
  // M3 Standard spatial: zeta 0.9, k 700
  standard: {
    damping: 0.9,
    stiffness: 700,
    durationMs: 359,
    linear: "linear(0, 0.0222 2.38%, 0.0899 5.18%, 0.4994 16.73%, 0.68 22.9%, 0.8106 29.18%, 0.9015 36.05%, 0.9605 44.33%, 0.9912 54.83%, 1)",
    points: [[0, 0], [0.0238, 0.0222], [0.0518, 0.0899], [0.1673, 0.4994], [0.229, 0.68], [0.2918, 0.8106], [0.3605, 0.9015], [0.4433, 0.9605], [0.5483, 0.9912], [1, 1]],
    maxError: 0.00707,
  },
};
