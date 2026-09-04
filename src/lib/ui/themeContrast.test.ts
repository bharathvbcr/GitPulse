import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");
const lightCss = css.match(/html\.light\s*\{([\s\S]*?)\n\}/)?.[1] ?? "";

function rgbVariable(name: string): [number, number, number] {
  const match = lightCss.match(new RegExp(`--${name}:\\s*(\\d+)\\s+(\\d+)\\s+(\\d+)`));
  if (!match) throw new Error(`missing --${name}`);
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

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

function composite(foreground: readonly number[], background: readonly number[], alpha: number) {
  return foreground.map((channel, index) =>
    Math.round(channel * alpha + background[index] * (1 - alpha)),
  );
}

describe("light-theme contrast contract", () => {
  const lightHover: [number, number, number] = [236, 238, 248];

  it("keeps ordinary text and accent text above WCAG AA on the darkest light surface", () => {
    expect(contrast(rgbVariable("c-text-muted"), lightHover)).toBeGreaterThanOrEqual(4.5);
    expect(contrast(rgbVariable("c-accent"), lightHover)).toBeGreaterThanOrEqual(4.5);
  });

  it("keeps 80%-opacity control borders distinguishable at 3:1", () => {
    const rendered = composite(rgbVariable("c-border"), lightHover, 0.8);
    expect(contrast(rendered, lightHover)).toBeGreaterThanOrEqual(3);
  });

  it.each([
    "amber",
    "emerald",
    "rose",
    "red",
    "sky",
    "cyan",
    "purple",
    "green",
    "orange",
    "teal",
    "indigo",
    "blue",
    "yellow",
    "zinc",
    "slate",
  ])("keeps %s status text above WCAG AA", (name) => {
    expect(contrast(rgbVariable(`c-status-${name}`), lightHover)).toBeGreaterThanOrEqual(4.5);
  });

  it("removes opacity from light-theme semantic text utilities", () => {
    expect(css).toContain('html.light :where([class~="text-textMuted/40"]');
    expect(css).toContain('[class~="text-accent/80"]');
    expect(css).toContain('[class~="text-amber-400/80"]');
  });
});
