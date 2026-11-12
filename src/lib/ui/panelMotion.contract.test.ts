import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const read = (path: string) => readFileSync(join(root, path), "utf8");
const css = read("src/app.css");

/** The body of a CSS block that starts at `header` (first match). */
function blockOf(header: string): string {
  const start = css.indexOf(header);
  expect(start, header).toBeGreaterThan(-1);
  return css.slice(start, css.indexOf("\n}", start));
}

describe("lane D motion vocabulary in app.css", () => {
  it("stops gp-indeterminate and gp-dock-motion in BOTH reduced-motion lists", () => {
    // motionPreference.test.ts pins the two lists equal; this names what lane D added.
    for (const name of ["gp-indeterminate", "gp-dock-motion"]) {
      expect(css, name).toMatch(new RegExp(`html :is\\([^)]*\\.${name}\\b[^)]*\\) \\{\\s*animation: none !important;`));
      expect(css, name).toMatch(
        new RegExp(`html\\[data-motion="reduced"\\] :is\\([^)]*\\.${name}\\b[^)]*\\) \\{\\s*animation: none !important;`),
      );
    }
  });

  it("makes the commit table's loading strip a named utility, not an arbitrary animate-[…] value", () => {
    const utility = blockOf("@utility gp-indeterminate {");
    expect(utility).toContain("animation: gp-slide");
    expect(css).toContain("@keyframes gp-slide");
    const table = read("src/lib/components/CommitTable.svelte");
    expect(table).toContain("gp-indeterminate");
    expect(table).not.toMatch(/animate-\[gp-slide/);
  });

  it("slides the dock on transform and opacity only, with a shorter exit than entrance", () => {
    for (const name of ["gp-dock-in", "gp-dock-slide-in", "gp-dock-slide-out"]) {
      const frames = blockOf(`@keyframes ${name} {`);
      expect(frames, name).not.toMatch(/\b(width|height|margin|padding|top|left|bottom|filter|box-shadow|backdrop-filter)\s*:/);
    }
    expect(css).toContain("animation: gp-dock-slide-in var(--gp-dur-slow)");
    expect(css).toContain("animation: gp-dock-slide-out var(--gp-dock-exit-ms, 70ms)");
    expect(css).toContain("animation: gp-dock-in var(--gp-dur-base)");
  });
});

describe("sidebar edge in light", () => {
  it("steps the light sidebar off the workspace plate with a token fill, at the plate's own alpha", () => {
    const rule = blockOf("html.macos.light .gp-sidebar {");
    expect(rule).toContain("rgb(var(--c-surface-hover) / calc(0.6 + 0.4 * var(--gp-material, 0)))");
    // The workspace plate is white at 0.6 in light; the sidebar must not be.
    expect(css).toContain("--mac-plate-fill: rgb(255 255 255 / calc(0.6 + 0.4 * var(--gp-material, 0)));");
    // Reduced transparency redeclares the opaque fills later in the cascade, so it still wins.
    expect(css.indexOf("html.macos.light .gp-sidebar {")).toBeLessThan(css.indexOf("@media (prefers-reduced-transparency: reduce)"));
  });
});

describe("boot plate", () => {
  const html = read("index.html");
  const boot = read("public/theme-boot.js");

  it("paints from inline CSS before any script, in both themes", () => {
    expect(html).toContain('<style id="gp-boot-css">');
    expect(html).toContain('<div id="gp-boot" aria-hidden="true">');
    expect(html).toContain("html.light #gp-boot");
    // The two surface tokens, as in app.css (--c-bg, dark and light).
    expect(html).toContain("rgb(11 14 23)");
    expect(html).toContain("rgb(243 244 250)");
    expect(css).toContain("--c-bg: 11 14 23;");
    expect(css).toContain("--c-bg: 243 244 250;");
  });

  it("never blocks the app: no pointer events, hidden without scripting, removed by a timer", () => {
    expect(html).toMatch(/#gp-boot \{[^}]*pointer-events: none;/);
    expect(html).toMatch(/<noscript>\s*<style>\s*#gp-boot \{\s*display: none;/);
    expect(boot).toContain('getElementById("gp-boot")');
    expect(boot).toContain("6000");
  });

  it("honours reduced motion in the inline CSS", () => {
    expect(html).toMatch(/prefers-reduced-motion: reduce\)\s*\{\s*#gp-boot \{\s*transition: none !important;/);
  });

  it("reads the resolved theme class that theme-boot.js sets", () => {
    expect(boot).toContain('root.classList.add("light")');
    expect(boot).toContain("gitpulse_theme_preference");
  });
});
