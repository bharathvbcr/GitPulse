import { motionDuration, type MediaMatch } from "./easing";
import { BAKED_SPRINGS } from "./springs.generated";

/**
 * The motion vocabulary: one set of durations and easing names that CSS
 * (`--gp-dur-*`, `--gp-ease-*`) and TypeScript share. Components name a token;
 * they do not carry a millisecond or a curve of their own.
 *
 * Reduced motion is not decided here: `motionMs` defers to
 * `prefersReducedMotion` in easing.ts, so the system preference wins and the
 * in-app setting can only add reduction.
 */

/** Every duration token, in ascending order. The type below derives from this tuple, so the map keyed by it cannot omit or add one. */
export const DURATION_TOKENS = Object.freeze(["instant", "fast", "base", "slow", "emphasis"] as const);

export type DurationToken = (typeof DURATION_TOKENS)[number];

/** Hover tint and press / scrim and fade / pop, menu, view switch / dialog, sheet, dock / splash hand-off. */
export const DURATION_MS: Readonly<Record<DurationToken, number>> = Object.freeze({
  instant: 80,
  fast: 140,
  base: 220,
  slow: 320,
  emphasis: 480,
});

/**
 * An exit yields faster than its entrance: half the enter duration, capped at
 * half of `fast`. Long outros made a close-then-reopen toggle stack two
 * blurred backdrops mid-fade, so exits get out of the way almost at once
 * (the invariant transitions.ts pins for its constants).
 */
export const EXIT_CAP_MS = DURATION_MS.fast / 2;

export const EXIT_MS: Readonly<Record<DurationToken, number>> = Object.freeze({
  instant: Math.min(DURATION_MS.instant / 2, EXIT_CAP_MS),
  fast: Math.min(DURATION_MS.fast / 2, EXIT_CAP_MS),
  base: Math.min(DURATION_MS.base / 2, EXIT_CAP_MS),
  slow: Math.min(DURATION_MS.slow / 2, EXIT_CAP_MS),
  emphasis: Math.min(DURATION_MS.emphasis / 2, EXIT_CAP_MS),
});

/** Every easing token. The type derives from this tuple, so the map keyed by it cannot omit or add one. */
export const EASING_TOKENS = Object.freeze(["spatialFast", "spatial", "spatialSlow", "effect", "exit"] as const);

export type EasingToken = (typeof EASING_TOKENS)[number];

/**
 * Value of `--mac-liquid-ease` in app.css. Engines without CSS `linear()` fall
 * back to it for every spring token; it does not overshoot, so `effect` stays
 * monotonic there too. A test pins this to the value in app.css.
 */
export const LIQUID_EASE_FALLBACK = "cubic-bezier(0.22, 1, 0.36, 1)";

export const EXIT_EASE = "cubic-bezier(0.4, 0, 1, 1)";

/** Baked `linear()` easing per token (exit is a plain cubic-bezier). */
export const EASING: Readonly<Record<EasingToken, string>> = Object.freeze({
  spatialFast: BAKED_SPRINGS.spatialFast.linear,
  spatial: BAKED_SPRINGS.spatial.linear,
  spatialSlow: BAKED_SPRINGS.spatialSlow.linear,
  effect: BAKED_SPRINGS.effect.linear,
  exit: EXIT_EASE,
});

/** What each token resolves to where `linear()` is not supported. */
export const EASING_FALLBACK: Readonly<Record<EasingToken, string>> = Object.freeze({
  spatialFast: LIQUID_EASE_FALLBACK,
  spatial: LIQUID_EASE_FALLBACK,
  spatialSlow: LIQUID_EASE_FALLBACK,
  effect: LIQUID_EASE_FALLBACK,
  exit: EXIT_EASE,
});

/** Duration in ms, or 0 when the user (system or in-app) asked for less motion. */
export function motionMs(token: DurationToken, media?: MediaMatch | null): number {
  return motionDuration(DURATION_MS[token], media);
}

/** Exit duration in ms (always shorter than the enter), or 0 under reduced motion. */
export function motionExitMs(token: DurationToken, media?: MediaMatch | null): number {
  return motionDuration(EXIT_MS[token], media);
}

function kebab(name: string): string {
  return name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
}

export function durationVariable(token: DurationToken): string {
  return `--gp-dur-${token}`;
}

export function easingVariable(token: EasingToken): string {
  return `--gp-ease-${kebab(token)}`;
}

/**
 * The `:root` declarations (one per token, no wrapper) for app.css:
 * `--gp-dur-*` and `--gp-ease-*`. Every easing carries its non-`linear()`
 * fallback here, so this block is safe on any engine; paste
 * `cssSpringOverrides()` after it to upgrade the springs where supported.
 */
export function cssVariables(): string {
  const lines: string[] = [];
  for (const token of DURATION_TOKENS) lines.push(`${durationVariable(token)}: ${DURATION_MS[token]}ms;`);
  for (const token of EASING_TOKENS) lines.push(`${easingVariable(token)}: ${EASING_FALLBACK[token]};`);
  return lines.join("\n");
}

/**
 * The `@supports` block that swaps each spring token's fallback for its baked
 * `linear()` curve. Defines only easing variables, each once; paste it after
 * the `:root` block from `cssVariables()`.
 */
export function cssSpringOverrides(): string {
  const springTokens = EASING_TOKENS.filter((token) => EASING[token].startsWith("linear("));
  const body = springTokens.map((token) => `    ${easingVariable(token)}: ${EASING[token]};`);
  return ["@supports (transition-timing-function: linear(0, 1)) {", "  :root {", ...body, "  }", "}"].join("\n");
}
