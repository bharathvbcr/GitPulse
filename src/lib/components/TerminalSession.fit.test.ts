import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { buildTermTheme, sameTheme, shouldRefit } from "./TerminalSession.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalSession.svelte"),
  "utf8",
);

/** The colour forms xterm 6.0.0's `css.toColor` reads without its canvas probe (Color.ts: `#rgb[a]`, `#rrggbb[aa]`, comma `rgb[a]()`). */
const parsedWithoutProbe = (value: string) =>
  /^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(value)
  || /^rgba?\(\s*\d{1,3}\s*,\s*\d{1,3}\s*,\s*\d{1,3}\s*(?:,\s*(?:0|1|\d?\.\d+)\s*)?\)$/.test(value);

describe("TerminalSession fit: a terminal nobody can see is never resized", () => {
  const live = { cols: 80, rows: 24 };

  it("refuses any proposal while the terminal has no box", () => {
    // Measured in Chrome and WebKit: inside `display: none` the fit addon
    // reads the declared `100%` as 100 pixels and proposes 11 x 6.
    expect(shouldRefit(live, { cols: 11, rows: 6 }, false)).toBe(false);
    expect(shouldRefit(null, { cols: 11, rows: 6 }, false)).toBe(false);
    expect(shouldRefit(live, { cols: 120, rows: 40 }, false)).toBe(false);
  });

  it("still refits a terminal that has a box, as before", () => {
    expect(shouldRefit(live, { cols: 120, rows: 40 }, true)).toBe(true);
    expect(shouldRefit(live, { cols: 120, rows: 40 })).toBe(true);
    expect(shouldRefit(live, { cols: 80, rows: 24 }, true)).toBe(false);
  });

  it("asks whether the box exists before it measures, in the one function that fits", () => {
    const body = source.slice(source.indexOf("function refitIfResized"), source.indexOf("function launcherConfig"));
    expect(body).toContain("hasRenderedBox(container)");
    expect(body.indexOf("hasRenderedBox(container)")).toBeLessThan(body.indexOf("proposeDimensions()"));
    expect(body).toMatch(/shouldRefit\([^)]*,\s*rendered\)/);
  });

  it("has exactly one place that resizes the emulator", () => {
    expect(source.match(/fitAddon\.fit\(\)/g)?.length).toBe(1);
    expect(source.match(/\bterm\.resize\(/g)?.length).toBe(1);
  });

  it("refits on every screen that shows, not only the active tab", () => {
    // A split's second half is on screen and not active.
    expect(source).not.toMatch(/disposed && active\) refitIfResized/);
    expect(source).not.toMatch(/active\) refitIfResized/);
  });

  it("fits a terminal that is being shown after the layout that shows it, once", () => {
    const body = source.slice(source.indexOf("export function reveal()"), source.indexOf("export async function openFind"));
    expect(body).toContain("fits.settle(");
    expect(body).not.toContain("refitIfResized()");
  });

  it("measures again when the display's pixel ratio changes, then clears the glyph atlas", () => {
    expect(source).toContain("watchDevicePixelRatio(");
    expect(source).toMatch(/fits\.settle\(\(\) => term\?\.clearTextureAtlas\(\)\)/);
  });
});

describe("TerminalSession theme", () => {
  const read = (name: string, fallback: string) =>
    ({
      "--bg-terminal": "rgb(20 26 41)",
      "--text-primary": "rgb(233 237 248)",
      "--accent-color": "rgb(128 158 255)",
      "--bg-surface": "rgb(20 26 41)",
    })[name] ?? fallback;

  it("gives xterm a selection colour its parser reads", () => {
    // `rgb(128 158 255 / 0.32)` is neither hex nor the comma form; xterm sends
    // it to a canvas probe that throws on alpha, and the default (translucent
    // white) is used instead.
    const theme = buildTermTheme(read);
    expect(parsedWithoutProbe(theme.selectionBackground)).toBe(true);
    expect(theme.selectionBackground).toBe("#809eff52");
  });

  it("gives xterm a surface colour its parser reads, translucent or not", () => {
    expect(parsedWithoutProbe(buildTermTheme(read).background)).toBe(true);
    expect(parsedWithoutProbe(buildTermTheme((n, f) => (n === "--bg-terminal" ? "rgba(20, 26, 41, 0.5)" : f)).background)).toBe(true);
    expect(buildTermTheme((n, f) => (n === "--bg-terminal" ? "transparent" : f)).background).toBe("#00000000");
  });

  it("falls back to the default palette when a token is missing", () => {
    const theme = buildTermTheme((_name, fallback) => fallback);
    expect(theme.background).toBe("#141a29");
    expect(theme.foreground).toBe("#e9edf8");
    expect(theme.cursor).toBe("#809eff");
  });

  it("treats a theme with the same colours as unchanged", () => {
    const a = buildTermTheme(read);
    expect(sameTheme(a, buildTermTheme(read))).toBe(true);
    expect(sameTheme(a, { ...a })).toBe(true);
  });

  it("treats any changed, added or removed colour as a change", () => {
    const a = buildTermTheme(read);
    expect(sameTheme(a, { ...a, cursor: "#000000" })).toBe(false);
    expect(sameTheme(a, { ...a, extra: "#000000" })).toBe(false);
    const { cursor: _cursor, ...fewer } = a;
    expect(sameTheme(a, fewer)).toBe(false);
    expect(sameTheme(null, a)).toBe(false);
  });

  it("applies a theme only when it differs from the one on the terminal", () => {
    // Accent, tab-size and UI-scale changes touch `style` on the root, which
    // the observer sees; each application refreshes the glyph atlas.
    const body = source.slice(source.indexOf("function applyTheme"), source.indexOf("function applyTheme") + 400);
    expect(body).toContain("sameTheme(");
    expect(source).not.toMatch(/term\.options\.theme = termTheme\(\)/);
  });
});
