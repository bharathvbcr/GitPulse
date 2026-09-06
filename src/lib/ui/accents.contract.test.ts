import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  ACCENT_IDS,
  ACCENTS,
  DEFAULT_ACCENT,
  accentChannels,
  accentFor,
  applyAccent,
  isAccentId,
  type Theme,
} from "./accents";

const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

function luminance(rgb: readonly number[]): number {
  const [r, g, b] = rgb.map((channel) => {
    const value = channel / 255;
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(foreground: readonly number[], background: readonly number[]): number {
  const a = luminance(foreground);
  const b = luminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

/**
 * The worst surface for accent text in each theme.
 *
 * Accent text is drawn on backgrounds, cards and hover rows; `--c-surface-hover`
 * is the lightest of the three in dark and the darkest of the three in light,
 * so clearing AA against it clears it against the other two.
 */
const WORST_SURFACE: Record<Theme, [number, number, number]> = {
  dark: [31, 39, 58],
  light: [236, 238, 248],
};

/** Reads a `--c-*` triplet out of one selector block in app.css. */
function cssTriplet(selector: string, name: string): [number, number, number] {
  const start = css.indexOf(selector);
  expect(start, `${selector} missing`).toBeGreaterThan(-1);
  const block = css.slice(start, css.indexOf("\n}", start));
  const match = block.match(new RegExp(`--${name}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
  if (!match) throw new Error(`missing --${name} in ${selector}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

class FakeStyle {
  readonly values = new Map<string, string>();
  setProperty(name: string, value: string) {
    this.values.set(name, value);
  }
  removeProperty(name: string) {
    this.values.delete(name);
  }
}

describe("accent palette contrast", () => {
  // Derived from the catalog rather than listed: an accent added without a
  // legible pair fails here instead of shipping unreadable text.
  const cases = ACCENTS.flatMap((accent) =>
    (["dark", "light"] as const).map((theme) => [accent.id, theme] as const),
  );

  it.each(cases)("keeps %s accent text above WCAG AA in the %s theme", (id, theme) => {
    const rgb = accentFor(id).rgb[theme];
    expect(contrast(rgb, WORST_SURFACE[theme])).toBeGreaterThanOrEqual(4.5);
  });

  it.each(cases)("keeps the %s accent usable as a 3:1 control edge in %s", (id, theme) => {
    // Borders and focus rings are non-text: AA needs 3:1 for those, and an
    // accent that only cleared the text bar could still vanish as an outline.
    const rgb = accentFor(id).rgb[theme];
    expect(contrast(rgb, WORST_SURFACE[theme])).toBeGreaterThanOrEqual(3);
  });

  it("keeps the id tuple and the palette in step, in both directions", () => {
    // The tuple is what the type is built from and what `isAccentId` checks;
    // the palette is what `accentFor` reads. An id in one and not the other
    // means a value that validates and then silently resolves to blue.
    expect([...ACCENT_IDS]).toEqual(ACCENTS.map((accent) => accent.id));
    for (const id of ACCENT_IDS) expect(accentFor(id).id).toBe(id);
  });

  it("gives every accent a distinct id and a label", () => {
    expect(new Set(ACCENTS.map((a) => a.id)).size).toBe(ACCENTS.length);
    for (const accent of ACCENTS) expect(accent.label.trim()).not.toBe("");
  });
});

describe("the default accent is the stylesheet's own", () => {
  // If these drift, "Blue" would silently mean a different colour from the
  // one a fresh install paints, and the applier's remove-the-override path
  // would change the window instead of restoring it.
  it.each([
    ["html.dark", "dark"],
    ["html.light", "light"],
  ] as const)("matches --c-accent declared under %s", (selector, theme) => {
    expect(accentFor(DEFAULT_ACCENT).rgb[theme]).toEqual(cssTriplet(selector, "c-accent"));
  });

  it("derives the glow and focus ring from --c-accent so they follow the choice", () => {
    // Hard-coded channels here would leave a teal window with a blue focus
    // ring — the tell that the setting is only half wired.
    for (const selector of ["html.dark", "html.light"]) {
      const start = css.indexOf(selector);
      const block = css.slice(start, css.indexOf("\n}", start));
      expect(block, `${selector} --shadow-glow`).toMatch(
        /--shadow-glow:[^;]*rgb\(var\(--c-accent\)/,
      );
      expect(block, `${selector} --ring-focus`).toMatch(
        /--ring-focus:[^;]*rgb\(var\(--c-accent\)/,
      );
    }
  });
});

describe("applyAccent", () => {
  it("writes the theme's channels for a non-default accent", () => {
    const style = new FakeStyle();
    applyAccent("teal", "dark", { style });
    expect(style.values.get("--c-accent")).toBe(accentChannels("teal", "dark"));

    applyAccent("teal", "light", { style });
    expect(style.values.get("--c-accent")).toBe(accentChannels("teal", "light"));
  });

  it("removes the override for the default, handing the theme back to app.css", () => {
    const style = new FakeStyle();
    applyAccent("teal", "dark", { style });
    applyAccent(DEFAULT_ACCENT, "dark", { style });
    expect(style.values.has("--c-accent")).toBe(false);
  });

  it("treats an unknown id as the default rather than writing garbage", () => {
    const style = new FakeStyle();
    applyAccent("teal", "dark", { style });
    applyAccent("chartreuse", "dark", { style });
    expect(style.values.has("--c-accent")).toBe(false);
    expect(isAccentId("chartreuse")).toBe(false);
  });

  it("is a no-op without a target instead of throwing", () => {
    expect(() => applyAccent("teal", "dark", null)).not.toThrow();
  });
});
