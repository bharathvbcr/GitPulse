import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The GitPulse mark is written out four times, because three of its users
 * cannot import a component: index.html paints the boot screen before any
 * script has loaded, and the two files in public/ are the favicon and the
 * source of the app icons. A redraw that reaches only some of them ships two
 * logos, so this holds every copy to the one in Logo.svelte.
 */
const read = (path: string): string => readFileSync(new URL(path, import.meta.url), "utf8");

const component = read("./Logo.svelte");
const copies: ReadonlyArray<readonly [string, string]> = [
  ["index.html", read("../../../index.html")],
  ["public/gitpulse-mark.svg", read("../../../public/gitpulse-mark.svg")],
  ["public/gitpulse-icon.svg", read("../../../public/gitpulse-icon.svg")],
];

/** The first capture of `pattern` in `text`, or a failure that names what was missing. */
function capture(text: string, pattern: RegExp, what: string): string {
  const hit = pattern.exec(text);
  if (!hit) throw new Error(`no ${what}`);
  return hit[1];
}

const trunk = (text: string) => capture(text, /<path\s[^>]*?\bd="([^"]+)"[^>]*stroke-linejoin="round"/, "stroked trunk path");
const node = (text: string) => {
  const hit = /<circle cx="([\d.]+)" cy="([\d.]+)" r="([\d.]+)"/.exec(text);
  if (!hit) throw new Error("no node circle");
  return hit.slice(1, 4).join(" ");
};
const stops = (text: string) => {
  const gradient = capture(text, /<linearGradient id="[^"]*stroke"[^>]*>([\s\S]*?)<\/linearGradient>/, "stroke gradient");
  return [...gradient.matchAll(/stop-color="(#[0-9a-f]{6})"/g)].map((m) => m[1]);
};
const badgeInset = capture(component, /variant === "badge" \? "([^"]+)"/, "badge inset in Logo.svelte");

describe("the GitPulse mark's copies", () => {
  it("has a drawing to compare against", () => {
    expect(trunk(component)).toMatch(/^M[\d.]+ [\d.]+A/);
    expect(stops(component)).toHaveLength(3);
  });

  it.each(copies)("%s draws the same stroke, node and gradient as Logo.svelte", (_name, text) => {
    expect(trunk(text)).toBe(trunk(component));
    expect(node(text)).toBe(node(component));
    expect(stops(text)).toEqual(stops(component));
  });

  it.each(copies)("%s insets the drawing as the badge does", (_name, text) => {
    expect(capture(text, /<g transform="([^"]+)"/, "art transform")).toBe(badgeInset);
  });

  it("traces by a normalised length, so the dash cannot go stale when the path is redrawn", () => {
    expect(component).toContain('pathLength="100"');
    expect(component).toMatch(/stroke-dasharray:\s*100;[\s\S]*stroke-dashoffset:\s*100;/);
  });

  it("gives the app icon a full square plate and the favicon a rounded one", () => {
    const mark = copies[1][1];
    const icon = copies[2][1];
    expect(icon).toContain('<rect width="32" height="32" fill="url(#gp-badge)"/>');
    expect(mark).toContain('<rect width="32" height="32" rx="10" fill="url(#gp-badge)"/>');
    // The plate must be measured in the viewBox's units, not the output's
    // pixels: a 1024-unit rect in a 32-unit box paints over every corner.
    expect(icon).not.toMatch(/<rect width="1024"/);
  });
});
