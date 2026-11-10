import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it } from "vitest";
import {
  BACKDROP_IN_MS,
  BACKDROP_OUT_MS,
  CARD_IN_MS,
  CARD_OUT_MS,
} from "../ui/transitions";
import { setReduceMotionOverride, type MediaMatch } from "./easing";
import { BAKED_SPRINGS } from "./springs.generated";
import {
  DURATION_MS,
  DURATION_TOKENS,
  EASING,
  EASING_FALLBACK,
  EASING_TOKENS,
  EXIT_CAP_MS,
  EXIT_EASE,
  EXIT_MS,
  LIQUID_EASE_FALLBACK,
  cssSpringOverrides,
  cssVariables,
  durationVariable,
  easingVariable,
  motionExitMs,
  motionMs,
  type DurationToken,
} from "./tokens";

const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

function media(matches: boolean): MediaMatch {
  return { matchMedia: () => ({ matches }) as MediaQueryList };
}

afterEach(() => setReduceMotionOverride(false));

describe("durations", () => {
  it("are the documented values", () => {
    expect(DURATION_MS).toEqual({ instant: 80, fast: 140, base: 220, slow: 320, emphasis: 480 });
  });

  it("are ordered instant < fast < base < slow < emphasis", () => {
    const values = DURATION_TOKENS.map((token) => DURATION_MS[token]);
    expect(DURATION_TOKENS).toEqual(["instant", "fast", "base", "slow", "emphasis"]);
    for (let i = 1; i < values.length; i += 1) {
      expect(values[i] as number).toBeGreaterThan(values[i - 1] as number);
    }
  });

  it("are frozen", () => {
    expect(Object.isFrozen(DURATION_MS)).toBe(true);
    expect(Object.isFrozen(EXIT_MS)).toBe(true);
    expect(Object.isFrozen(EASING)).toBe(true);
    expect(() => {
      (DURATION_MS as Record<string, number>).base = 1;
    }).toThrow();
  });
});

describe("the exit invariant", () => {
  it("makes every exit strictly shorter than its enter", () => {
    for (const token of DURATION_TOKENS) {
      expect(EXIT_MS[token], token).toBeLessThan(DURATION_MS[token]);
      expect(EXIT_MS[token], token).toBeGreaterThan(0);
      expect(EXIT_MS[token], token).toBeLessThanOrEqual(EXIT_CAP_MS);
    }
  });

  it("agrees with the shorter-OUT-than-IN constants transitions.ts already pins", () => {
    expect(BACKDROP_OUT_MS).toBeLessThan(BACKDROP_IN_MS);
    expect(CARD_OUT_MS).toBeLessThan(CARD_IN_MS);
    // The scrim and card enters sit on the vocabulary; their exits stay under the cap.
    expect(BACKDROP_IN_MS).toBe(DURATION_MS.fast);
    expect(BACKDROP_OUT_MS).toBeLessThanOrEqual(EXIT_CAP_MS);
    expect(CARD_OUT_MS).toBeLessThanOrEqual(EXIT_CAP_MS);
  });
});

describe("easings", () => {
  it("name the five documented curves", () => {
    expect([...EASING_TOKENS]).toEqual(["spatialFast", "spatial", "spatialSlow", "effect", "exit"]);
  });

  it("map the springs to their baked linear() curves and exit to a cubic-bezier", () => {
    expect(EASING.spatialFast).toBe(BAKED_SPRINGS.spatialFast.linear);
    expect(EASING.spatial).toBe(BAKED_SPRINGS.spatial.linear);
    expect(EASING.spatialSlow).toBe(BAKED_SPRINGS.spatialSlow.linear);
    expect(EASING.effect).toBe(BAKED_SPRINGS.effect.linear);
    expect(EASING.exit).toBe("cubic-bezier(0.4, 0, 1, 1)");
    expect(EXIT_EASE).toBe(EASING.exit);
    for (const token of EASING_TOKENS) {
      if (token !== "exit") expect(EASING[token], token).toMatch(/^linear\(0, .+, 1\)$/);
    }
  });

  it("fall back to app.css's --mac-liquid-ease where linear() is unsupported", () => {
    const declared = /--mac-liquid-ease:\s*([^;]+);/.exec(css)?.[1]?.trim();
    expect(declared).toBe(LIQUID_EASE_FALLBACK);
    for (const token of EASING_TOKENS) {
      expect(EASING_FALLBACK[token], token).toBe(token === "exit" ? EXIT_EASE : LIQUID_EASE_FALLBACK);
      expect(EASING_FALLBACK[token], token).not.toContain("linear(");
    }
  });
});

describe("motionMs", () => {
  it("returns the token's duration when motion is allowed", () => {
    for (const token of DURATION_TOKENS) {
      expect(motionMs(token, media(false))).toBe(DURATION_MS[token]);
      expect(motionExitMs(token, media(false))).toBe(EXIT_MS[token]);
    }
  });

  it("returns 0 for every token when the system asks for reduced motion", () => {
    for (const token of DURATION_TOKENS) {
      expect(motionMs(token, media(true))).toBe(0);
      expect(motionExitMs(token, media(true))).toBe(0);
    }
  });

  it("returns 0 for every token when only the in-app setting asks for it", () => {
    setReduceMotionOverride(true);
    for (const token of DURATION_TOKENS) expect(motionMs(token, media(false))).toBe(0);
  });

  it("does not let the in-app setting being off override the system", () => {
    setReduceMotionOverride(false);
    expect(motionMs("base", media(true))).toBe(0);
  });
});

describe("cssVariables", () => {
  const declarations = cssVariables().split("\n");
  const names = declarations.map((line) => /^(--[a-z-]+):/.exec(line)?.[1]);

  it("defines every token name exactly once", () => {
    const expected = [
      ...DURATION_TOKENS.map(durationVariable),
      ...EASING_TOKENS.map(easingVariable),
    ];
    expect(names.every((name) => name !== undefined)).toBe(true);
    expect([...names].sort()).toEqual([...expected].sort());
    expect(new Set(names).size).toBe(names.length);
    expect(expected).toContain("--gp-ease-spatial-fast");
    expect(expected).toContain("--gp-ease-exit");
    expect(expected).toContain("--gp-dur-emphasis");
  });

  it("writes durations in ms and gives every easing a non-linear() fallback", () => {
    for (const token of DURATION_TOKENS as readonly DurationToken[]) {
      expect(cssVariables()).toContain(`${durationVariable(token)}: ${DURATION_MS[token]}ms;`);
    }
    for (const token of EASING_TOKENS) {
      expect(cssVariables()).toContain(`${easingVariable(token)}: ${EASING_FALLBACK[token]};`);
    }
    expect(cssVariables()).not.toContain("linear(");
    for (const line of declarations) expect(line.endsWith(";")).toBe(true);
  });

  it("upgrades exactly the four springs inside one @supports block, once each", () => {
    const overrides = cssSpringOverrides();
    expect(overrides.startsWith("@supports (transition-timing-function: linear(0, 1)) {")).toBe(true);
    const defined = [...overrides.matchAll(/^\s+(--gp-[a-z-]+):\s*(.+);$/gm)];
    expect(defined.map((m) => m[1]).sort()).toEqual(
      ["--gp-ease-effect", "--gp-ease-spatial", "--gp-ease-spatial-fast", "--gp-ease-spatial-slow"],
    );
    for (const m of defined) expect(m[2]).toMatch(/^linear\(/);
    expect(overrides).not.toContain("--gp-dur-");
    expect(overrides).not.toContain("--gp-ease-exit");
    expect(overrides).toContain(`${easingVariable("spatial")}: ${EASING.spatial};`);
  });
});
