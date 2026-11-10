import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { NON_TEXT_MIN, TEXT_MIN } from "./color/guard";
import { contrastRatio, type Rgb } from "./color/oklch";

/**
 * The shared-primitives lane's stylesheet additions (design language §9):
 * `gp-notice` (the honesty surface) and `gp-field-multi` (the multiline field).
 * Read from the stylesheet that ships, like foundationTokens.contract.test.ts.
 */
const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

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

const TONES = ["ok", "warn", "danger", "info", "unknown"] as const;
/** The tint a notice lays over --c-surface. The contrast tests below measure exactly this alpha. */
const TINT = 0.06;
const notice = blockOf("@utility gp-notice {");
const field = blockOf("@utility gp-field-multi {");

describe("gp-notice", () => {
  it("has one rule per tone, each pointing --gp-notice-tone at its status role", () => {
    for (const tone of TONES) {
      const rule = new RegExp(`&\\[data-tone="${tone}"\\]\\s*\\{[^}]*--gp-notice-tone:\\s*var\\(--c-${tone}\\);`);
      expect(notice, tone).toMatch(rule);
    }
  });

  it("tints from the tone over an opaque surface, so it needs no Reduce Transparency twin", () => {
    const toned = blockOf("&[data-tone] {");
    expect(toned).toContain("background-color: rgb(var(--c-surface));");
    expect(toned).toContain(`linear-gradient(rgb(var(--gp-notice-tone) / ${TINT}), rgb(var(--gp-notice-tone) / ${TINT}))`);
    expect(toned).toContain("border-color: rgb(var(--gp-notice-tone) / 0.35);");
    // Body text is the primary ink; the tone itself is exposed as ink for an icon or title.
    expect(toned).toContain("color: rgb(var(--c-text));");
    expect(toned).toContain("--gp-notice-ink: rgb(var(--gp-notice-tone));");
  });

  it("is a rounded rectangle on the md radius, laid out as icon, body, action", () => {
    const toned = blockOf("&[data-tone] {");
    expect(toned).toContain("border-radius: var(--gp-r-md);");
    expect(toned).toMatch(/display:\s*flex;[^}]*align-items:\s*flex-start;/);
    expect(toned).toContain("gap: var(--gp-space-2);");
    // The legacy margins belong to the old bare form; a toned notice sits in its parent's flow.
    expect(toned).toContain("margin: 0;");
  });

  it("marks unknown with a dashed edge, so it is told apart without color", () => {
    expect(blockOf('&[data-tone="unknown"] {')).toContain("border-style: dashed;");
    for (const tone of ["ok", "warn", "danger", "info"]) {
      expect(blockOf(`&[data-tone="${tone}"] {`), tone).not.toContain("border-style");
    }
  });

  it("uses role tokens only: no palette color, hex or rgb triplet literal", () => {
    const toned = TONES.map((tone) => blockOf(`&[data-tone="${tone}"] {`)).join("\n") + blockOf("&[data-tone] {");
    expect(toned).not.toMatch(/#[0-9a-fA-F]{3,8}\b|\brgb\(\s*\d|\b(?:zinc|slate|gray|amber|rose|emerald|sky|red|green|blue)-\d/);
  });

  it("animates nothing, so it adds no reduced-motion entry", () => {
    expect(notice).not.toMatch(/transition|animation|@keyframes/);
  });

  it("keeps the bare form and the legacy modifiers working for the call sites that still use them", () => {
    expect(notice).toContain("@apply mx-3 mt-2 rounded-xl border border-border/65 bg-surface/60 px-3 py-2 text-xs;");
    for (const legacy of ["gp-notice-error", "gp-notice-warn"]) expect(css).toMatch(new RegExp(`^@utility ${legacy} \\{`, "m"));
  });
});

describe("gp-notice contrast, both themes", () => {
  const darkStart = css.indexOf("html.dark {");
  const blocks = {
    dark: css.slice(darkStart, css.indexOf("\n  }", darkStart)),
    light: css.match(/html\.light\s*\{([\s\S]*?)\n\s*\}/)?.[1] ?? "",
  } as const;

  function triplet(theme: "dark" | "light", name: string): Rgb {
    const match = blocks[theme].match(new RegExp(`--${name}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
    if (!match) throw new Error(`missing --${name} in the ${theme} theme`);
    return [Number(match[1]), Number(match[2]), Number(match[3])];
  }

  /** `foreground` laid over `background` at `alpha`, unrounded like the browser. */
  function over(foreground: Rgb, background: Rgb, alpha: number): Rgb {
    return [0, 1, 2].map((i) => foreground[i] * alpha + background[i] * (1 - alpha)) as unknown as Rgb;
  }

  it("finds both theme blocks it is meant to measure", () => {
    expect(blocks.dark).toContain("--c-ok:");
    expect(blocks.light).toContain("--c-ok:");
  });

  describe.each(["dark", "light"] as const)("%s theme", (theme) => {
    // The fill is the tone at TINT over --c-surface; that is the ground for everything inside.
    const fill = (tone: string) => over(triplet(theme, `c-${tone}`), triplet(theme, "c-surface"), TINT);

    it.each(TONES)("keeps body text at 4.5:1 on a %s notice", (tone) => {
      expect(contrastRatio(triplet(theme, "c-text"), fill(tone))).toBeGreaterThanOrEqual(TEXT_MIN);
    });

    it.each(TONES)("keeps secondary text at 4.5:1 on a %s notice", (tone) => {
      expect(contrastRatio(triplet(theme, "c-text-muted"), fill(tone))).toBeGreaterThanOrEqual(TEXT_MIN);
      expect(contrastRatio(triplet(theme, "c-text-faint"), fill(tone))).toBeGreaterThanOrEqual(TEXT_MIN);
    });

    it.each(TONES)("keeps the %s tone readable as ink (title, icon) on its own fill at 4.5:1", (tone) => {
      expect(contrastRatio(triplet(theme, `c-${tone}`), fill(tone))).toBeGreaterThanOrEqual(TEXT_MIN);
    });

    it.each(TONES)("keeps accent text, as in a link inside a %s notice, at 4.5:1", (tone) => {
      expect(contrastRatio(triplet(theme, "c-accent"), fill(tone))).toBeGreaterThanOrEqual(TEXT_MIN);
    });

    it("keeps the danger edge of an invalid field at 3:1 against every surface", () => {
      for (const surface of ["c-bg", "c-surface", "c-surface-hover"]) {
        expect(contrastRatio(triplet(theme, "c-danger"), triplet(theme, surface))).toBeGreaterThanOrEqual(NON_TEXT_MIN);
      }
    });
  });
});

describe("gp-field-multi", () => {
  it("wears the whole gp-field, so it works with or without it on the element", () => {
    expect(field).toContain("@apply gp-field;");
  });

  it("is a rounded rectangle, and beats gp-field's pill whichever order the two are emitted in", () => {
    // Tailwind emits gp-field after gp-field-multi, so a single-class rule loses the tie.
    const shape = blockOf("&& {");
    expect(shape).toContain("border-radius: var(--gp-r-md);");
    expect(shape).toContain("padding-block: var(--gp-space-2);");
  });

  it("turns the edge and the focus ring to the danger role when invalid", () => {
    expect(field).toContain('&[data-invalid]:not([data-invalid="false"]),');
    expect(field).toContain('&[aria-invalid="true"] {');
    expect(field).toMatch(/border-color:\s*rgb\(var\(--c-danger\)\);/);
    expect(field).toContain("box-shadow: 0 0 0 3px rgb(var(--c-danger) / 0.28);");
  });

  it("dims and locks a disabled field", () => {
    const disabled = blockOf('&:disabled,\n  &[aria-disabled="true"] {');
    expect(disabled).toContain("@apply cursor-not-allowed opacity-40;");
    expect(disabled).toContain("resize: none;");
  });

  it("keeps gp-field's focus: the accent edge and --ring-focus, inherited, not restated", () => {
    expect(blockOf("@utility gp-field {")).toContain("box-shadow: var(--ring-focus);");
    expect(field.replace(/\/\*[\s\S]*?\*\//g, "")).not.toContain("--ring-focus");
  });

  it("is in both reduced-motion lists and the macOS field fill, beside gp-field", () => {
    expect(css).toMatch(/html :is\([^)]*\.gp-field, \.gp-field-multi, \.gp-slide-docked[^)]*\) \{\s*transition: none !important;/);
    expect(css).toMatch(/html\[data-motion="reduced"\] :is\([^)]*\.gp-field, \.gp-field-multi, \.gp-slide-docked[^)]*\) \{\s*transition: none !important;/);
    expect(css).toContain('html.macos :where([class~="bg-background"], .gp-field, .gp-field-multi, .gp-select) {');
  });

  it("every existing call site still gets the multiline shape: gp-field gp-field-multi", () => {
    expect(css).toMatch(/^@utility gp-field-multi \{/m);
    expect(css).toMatch(/^@utility gp-field \{/m);
  });
});
