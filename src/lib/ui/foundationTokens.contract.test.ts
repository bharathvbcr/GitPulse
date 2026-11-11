import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { LAYERS } from "./layers";
import {
  DURATION_MS,
  DURATION_TOKENS,
  EASING_FALLBACK,
  EASING_TOKENS,
  cssSpringOverrides,
  cssVariables,
  durationVariable,
  easingVariable,
} from "../motion/tokens";

/**
 * The Wave 1 vocabulary in app.css (design language §3-§8): the tokens
 * components will be migrated onto, the utilities that wear them, and the proof
 * that nothing the sheet defined before this vocabulary was lost.
 *
 * Read from the stylesheet that ships. Where a value mirrors a TypeScript
 * module (motion tokens, layers) the comparison is against that module, so
 * editing either side alone fails here.
 */
const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

/** Body of the first `header { … }` block (balanced), or throws. */
function blockOf(header: string): string {
  const start = css.indexOf(header);
  if (start < 0) throw new Error(`app.css has no ${header}`);
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < css.length; i += 1) {
    if (css[i] === "{") depth += 1;
    if (css[i] === "}") {
      depth -= 1;
      if (depth === 0) return css.slice(open + 1, i);
    }
  }
  throw new Error(`unbalanced ${header}`);
}

function declarations(block: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const [, name, value] of block.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/(--[\w-]+):\s*([^;]+);/g)) {
    found.set(name, value.trim().replace(/\s+/g, " "));
  }
  return found;
}

describe("motion variables mirror src/lib/motion/tokens.ts", () => {
  const theme = declarations(blockOf("@theme static"));

  it("declares every duration and easing token once, with the value cssVariables() writes", () => {
    const expected = declarations(cssVariables());
    expect(expected.size).toBe(DURATION_TOKENS.length + EASING_TOKENS.length);
    for (const [name, value] of expected) expect(theme.get(name), name).toBe(value);
  });

  it("keeps the durations at 80 / 140 / 220 / 320 / 480 ms", () => {
    expect(DURATION_TOKENS.map((token) => theme.get(durationVariable(token)))).toEqual(
      ["80ms", "140ms", "220ms", "320ms", "480ms"],
    );
    for (const token of DURATION_TOKENS) expect(theme.get(durationVariable(token))).toBe(`${DURATION_MS[token]}ms`);
  });

  it("upgrades the springs to their baked linear() curves with the block cssSpringOverrides() writes", () => {
    const baked = declarations(cssSpringOverrides());
    const supports = declarations(blockOf("@supports (transition-timing-function: linear(0, 1))"));
    expect([...supports.keys()].sort()).toEqual(
      EASING_TOKENS.filter((token) => token !== "exit").map(easingVariable).sort(),
    );
    for (const [name, value] of baked) {
      expect(value).toMatch(/^linear\(/);
      expect(supports.get(name), name).toBe(value);
    }
    // And the fallback it replaces is the non-overshooting one tokens.ts names.
    for (const token of EASING_TOKENS) expect(theme.get(easingVariable(token))).toBe(EASING_FALLBACK[token]);
  });
});

describe("shape, space, density and edge tokens", () => {
  const theme = declarations(blockOf("@theme static"));

  it("declares the radius ladder of design language §5", () => {
    expect(["xs", "sm", "md", "lg", "xl", "full"].map((step) => theme.get(`--gp-r-${step}`))).toEqual(
      ["4px", "8px", "12px", "16px", "20px", "9999px"],
    );
    // The pane radius is the macOS one that already exists, with its own 14 as the fallback.
    expect(theme.get("--gp-r-pane")).toBe("var(--gp-pane-radius, 14px)");
    expect(css).toMatch(/html\.macos \{\s*--gp-pane-gutter:\s*0px;\s*--gp-pane-radius:\s*14px;/);
  });

  it("declares the 4px space steps", () => {
    expect([1, 2, 3, 4, 5, 6, 7, 8].map((step) => theme.get(`--gp-space-${step}`))).toEqual(
      ["4px", "8px", "12px", "16px", "20px", "24px", "32px", "40px"],
    );
  });

  it("declares density per §7: comfortable by default, compact and spacious on html[data-density]", () => {
    const root = declarations(blockOf("\n  :root {\n    /* The three border-opacity"));
    expect(["--gp-row-h", "--gp-header-h", "--gp-pad", "--gp-density"].map((name) => root.get(name))).toEqual(
      ["28px", "32px", "12px", "1"],
    );
    const table = {
      compact: ["24px", "28px", "8px", "0.85"],
      comfortable: ["28px", "32px", "12px", "1"],
      spacious: ["32px", "36px", "16px", "1.15"],
    } as const;
    for (const [name, want] of Object.entries(table)) {
      const found = declarations(blockOf(`html[data-density="${name}"]`));
      expect(["--gp-row-h", "--gp-header-h", "--gp-pad", "--gp-density"].map((key) => found.get(key)), name).toEqual([...want]);
    }
  });

  it("declares exactly three edge stops at 0.35, 0.65 and 0.9 of the border color", () => {
    const root = declarations(blockOf("\n  :root {\n    /* The three border-opacity"));
    expect(root.get("--gp-edge-soft")).toBe("rgb(var(--c-border) / 0.35)");
    expect(root.get("--gp-edge")).toBe("rgb(var(--c-border) / 0.65)");
    expect(root.get("--gp-edge-strong")).toBe("rgb(var(--c-border) / 0.9)");
    expect([...root.keys()].filter((name) => name.startsWith("--gp-edge"))).toHaveLength(3);
  });

  it("publishes Tailwind colors for every role and edge, so components write text-ok / bg-warn/10 / border-edge", () => {
    const themeBlock = declarations(blockOf("@theme {"));
    for (const role of ["faint", "onAccent", "ok", "onOk", "warn", "onWarn", "danger", "onDanger", "info", "onInfo", "unknown", "onUnknown"]) {
      expect(themeBlock.has(`--color-${role}`), `--color-${role}`).toBe(true);
    }
    expect(themeBlock.get("--color-faint")).toBe("rgb(var(--c-text-faint))");
    expect(themeBlock.get("--color-edgeSoft")).toBe("var(--gp-edge-soft)");
    expect(themeBlock.get("--color-edge")).toBe("var(--gp-edge)");
    expect(themeBlock.get("--color-edgeStrong")).toBe("var(--gp-edge-strong)");
  });

  it("defines the six-step type scale: micro and meta are new, sm and base move from 14 and 16 to 13 and 15, xs stays 12px, all static so var(--text-*) always resolves", () => {
    const themeBlock = declarations(blockOf("@theme static"));
    expect(themeBlock.get("--text-micro")).toBe("0.625rem");
    expect(themeBlock.get("--text-meta")).toBe("0.6875rem");
    expect(themeBlock.get("--text-micro--line-height")).toBe("1.3");
    expect(themeBlock.get("--text-meta--line-height")).toBe("1.4");
    expect(themeBlock.get("--text-sm")).toBe("0.8125rem");
    expect(themeBlock.get("--text-sm--line-height")).toBe("1.45");
    expect(themeBlock.get("--text-base")).toBe("0.9375rem");
    expect(themeBlock.get("--text-base--line-height")).toBe("1.3");
    expect(themeBlock.get("--text-lg")).toBe("1.125rem");
    expect(themeBlock.get("--text-lg--line-height")).toBe("1.25");
    expect(themeBlock.get("--text-xs")).toBe("0.75rem");
    expect(themeBlock.get("--text-xs--line-height")).toBe("calc(1 / 0.75)");
  });
});

describe("the vocabulary utilities", () => {
  it.each(["gp-btn", "gp-chip", "gp-header", "gp-header-actions", "gp-row", "gp-label", "gp-select", "gp-slide-docked"])(
    "defines @utility %s",
    (name) => {
      expect(css).toMatch(new RegExp(`^@utility ${name} \\{`, "m"));
    },
  );

  it("gives gp-btn every variant and both sizes", () => {
    const button = blockOf("@utility gp-btn {");
    for (const variant of ["primary", "danger", "success", "ghost"]) {
      expect(button, variant).toContain(`&[data-variant="${variant}"]`);
    }
    for (const size of ["sm", "md"]) expect(button, size).toContain(`&[data-size="${size}"]`);
    // 24px and 28px, the heights the design language names.
    expect(blockOf('&[data-size="sm"] {')).toContain("min-height: 24px");
    expect(blockOf('&[data-size="md"] {')).toContain("min-height: 28px");
    // Only colors and the press transform animate: no layout, no shadow, no filter.
    expect(button).toContain("transition-property: color, background-color, border-color, transform;");
    expect(button).toContain("transition-duration: var(--gp-dur-instant);");
  });

  it("gives gp-chip every tone and every kind", () => {
    const chip = blockOf("@utility gp-chip {");
    for (const tone of ["neutral", "accent", "ok", "warn", "danger", "info", "unknown"]) {
      expect(chip, tone).toContain(`&[data-tone="${tone}"]`);
    }
    for (const kind of ["count", "label", "removable"]) expect(chip, kind).toContain(`&[data-kind="${kind}"]`);
  });

  it("makes gp-select a pill with gp-field's metrics", () => {
    const select = blockOf("@utility gp-select {");
    expect(select).toMatch(/@apply [^;]*\brounded-full\b/);
    expect(select).not.toMatch(/\brounded-lg\b/);
    expect(select).toMatch(/\bpy-1\b/);
    expect(select).toMatch(/\bpl-3\b/);
  });

  it("caps gp-slide-docked at the fast token, under the 200 ms the spec allows, and animates only size", () => {
    const slide = blockOf("@utility gp-slide-docked {");
    expect(slide).toContain("transition-property: width, height;");
    expect(slide).toContain("transition-duration: var(--gp-dur-fast);");
    expect(DURATION_MS.fast).toBeLessThanOrEqual(200);
    // It is the single sanctioned layout transition: no other utility animates width or height.
    const others = [...css.matchAll(/^@utility ([\w-]+) \{/gm)].map((match) => match[1]).filter((name) => name !== "gp-slide-docked");
    for (const name of others) {
      expect(blockOf(`@utility ${name} {`), name).not.toMatch(/transition(?:-property)?:[^;]*\b(?:width|height|margin|padding|top|left|filter|box-shadow|backdrop-filter)\b/);
    }
  });

  it("zeroes every animated vocabulary class under reduced motion, in both lists", () => {
    // The pairing itself is pinned by motionPreference.test.ts; this names the classes this wave added.
    for (const name of ["gp-btn", "gp-chip", "gp-row", "gp-slide-docked"]) {
      expect(css, name).toMatch(new RegExp(`html :is\\([^)]*\\.${name}\\b[^)]*\\) \\{\\s*transition: none !important;`));
      expect(css, name).toMatch(new RegExp(`html\\[data-motion="reduced"\\] :is\\([^)]*\\.${name}\\b[^)]*\\) \\{\\s*transition: none !important;`));
    }
  });
});

describe("LAYERS", () => {
  it("adds TOAST 65, STICKY 10 and SCROLL_CUE 20", () => {
    expect(LAYERS.TOAST).toBe(65);
    expect(LAYERS.STICKY).toBe(10);
    expect(LAYERS.SCROLL_CUE).toBe(20);
  });

  it("keeps the existing tiers where they were", () => {
    expect({ DROP_OVERLAY: LAYERS.DROP_OVERLAY, TOUR: LAYERS.TOUR, MENU: LAYERS.MENU, MODAL: LAYERS.MODAL, PROMPT: LAYERS.PROMPT, TOOLTIP: LAYERS.TOOLTIP }).toEqual(
      { DROP_OVERLAY: 40, TOUR: 45, MENU: 50, MODAL: 50, PROMPT: 60, TOOLTIP: 70 },
    );
  });

  it("orders the new tiers without crossing an existing one", () => {
    expect(LAYERS.STICKY).toBeLessThan(LAYERS.SCROLL_CUE);
    expect(LAYERS.SCROLL_CUE).toBeLessThan(LAYERS.DROP_OVERLAY);
    // A toast clears a dialog and its prompt, and stays under the tooltip.
    expect(LAYERS.TOAST).toBeGreaterThan(LAYERS.PROMPT);
    expect(LAYERS.TOAST).toBeGreaterThan(LAYERS.MODAL);
    expect(LAYERS.TOAST).toBeLessThan(LAYERS.TOOLTIP);
  });

  it("has no two tiers share a value except the documented MENU / MODAL tie", () => {
    const byValue = new Map<number, string[]>();
    for (const [name, value] of Object.entries(LAYERS)) byValue.set(value, [...(byValue.get(value) ?? []), name]);
    const ties = [...byValue.values()].filter((names) => names.length > 1);
    expect(ties).toEqual([["MENU", "MODAL"]]);
  });

  it("matches the z-index gp-scroll-cue paints at in app.css", () => {
    const cues = ["gp-scroll-cue", "gp-scroll-cue-y"].map((name) => blockOf(`@utility ${name} {`));
    for (const cue of cues) expect(cue).toMatch(new RegExp(`\\bz-${LAYERS.SCROLL_CUE}\\b`));
  });
});

describe("nothing the sheet defined before the vocabulary was lost", () => {
  /** Every `@utility` at the Wave 1 base (overhaul/wave-1 = 13127387). */
  const BASE_UTILITIES = [
    "gp-btn", "gp-btn-primary", "gp-btn-danger", "gp-icon-btn", "gp-pill", "gp-card", "gp-scrim",
    "gp-scroll-cue", "gp-scroll-cue-start", "gp-scroll-cue-end", "gp-scroll-cue-y", "gp-scroll-cue-top",
    "gp-scroll-cue-bottom", "gp-scroll-cue-btn", "gp-menu", "gp-menu-item", "gp-separator", "gp-menu-sep",
    "gp-section-edge", "gp-segmented", "gp-surface-header", "gp-surface-title", "gp-notice", "gp-notice-error",
    "gp-notice-warn", "gp-seg-btn", "gp-field", "gp-field-multi", "gp-chip", "gp-floor", "gp-select", "gp-keycap",
  ];

  it.each(BASE_UTILITIES)("still defines @utility %s", (name) => {
    expect(css).toMatch(new RegExp(`^@utility ${name} \\{`, "m"));
  });

  it("keeps the gp-btn-* aliases as standalone working utilities (Wave 2 empties them, not this wave)", () => {
    for (const alias of ["gp-btn-primary", "gp-btn-danger"]) {
      const body = blockOf(`@utility ${alias} {`);
      expect(body, alias).toContain("rounded-full");
      expect(body, alias).toContain("background-image");
    }
  });

  it("keeps the theme triplets and no light-only status ramp and no class-attribute colour remap", () => {
    for (const name of ["bg", "surface", "surface-hover", "border", "accent", "text", "text-muted"]) {
      expect(css).toMatch(new RegExp(`--c-${name}:\\s*\\d+ \\d+ \\d+;`));
    }
    expect(css).not.toMatch(/--c-status-/);
    expect(css).not.toMatch(/\[class~=["']text-/);
  });

  it("keeps the macOS material, forced-colors and view-transition blocks", () => {
    for (const marker of [
      "html.macos {\n  /* The hue field.",
      "html.macos.light {\n  /* Light surfaces invert the problem",
      "@media (forced-colors: active)",
      "@media (prefers-contrast: more)",
      "::view-transition-old(root)",
      "@keyframes gp-view-in",
      "@keyframes gp-pop-in",
      "@keyframes gp-overlay-in",
      "@keyframes gp-slide",
      ".gp-markdown {",
    ]) {
      expect(css, marker).toContain(marker);
    }
  });
});
