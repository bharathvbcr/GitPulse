import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

function block(header: string): string {
  const start = css.indexOf(header);
  expect(start, `${header} missing`).toBeGreaterThan(-1);
  let depth = 0;
  for (let i = start; i < css.length; i += 1) {
    if (css[i] === "{") depth += 1;
    if (css[i] === "}") {
      depth -= 1;
      if (depth === 0) return css.slice(start, i + 1);
    }
  }
  throw new Error(`unterminated block for ${header}`);
}

describe("focus is visible when the system paints the colours", () => {
  /**
   * The base rule is `outline: none` plus a box-shadow ring. Forced-colours
   * mode suppresses box-shadow and does not restore the outline, which left
   * keyboard focus with no indicator at all on Windows High Contrast.
   */
  it("restores a real outline under forced colours", () => {
    const forced = block("@media (forced-colors: active)");
    expect(forced).toMatch(/outline:\s*2px solid Highlight/);
  });

  it("covers every role the base focus rule covers", () => {
    const base = block("  button:focus-visible");
    const forced = block("@media (forced-colors: active)");
    for (const role of [
      "button:focus-visible",
      '[role="button"]:focus-visible',
      '[role="tab"]:focus-visible',
      "input:focus-visible",
      "textarea:focus-visible",
      "select:focus-visible",
    ]) {
      expect(base).toContain(role);
      expect(forced).toContain(role);
    }
  });

  it("uses system colour keywords rather than theme tokens", () => {
    // In forced-colours mode the palette belongs to the user; a token would
    // be overridden and the rule would draw nothing.
    const forced = block("@media (forced-colors: active)");
    expect(forced).not.toContain("var(--c-accent)");
    expect(forced).toContain("CanvasText");
  });

  it("thickens the ring for a stated contrast preference", () => {
    expect(block("@media (prefers-contrast: more)")).toContain("--ring-focus");
  });
});

describe("stylesheet carries no rules the shipped webviews cannot reach", () => {
  it("has no Gecko-only pseudo-elements", () => {
    // Every GitPulse target is WebKit (macOS, Linux) or Chromium (Windows).
    // The dead block also disagreed with the live one on thumb size, which is
    // how a stale copy misleads the next person to touch it.
    expect(css).not.toContain("-moz-range-track");
    expect(css).not.toContain("-moz-range-thumb");
  });
});
