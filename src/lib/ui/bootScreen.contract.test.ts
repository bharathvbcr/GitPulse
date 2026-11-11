import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The boot screen is painted by index.html before any stylesheet or script has
 * loaded, so its two colours are literals. They must be the `--c-bg` the app
 * settles on, or the window flashes a different tone when the splash hands off.
 * This pins each literal to the token for its theme.
 */
const html = readFileSync(new URL("../../../index.html", import.meta.url), "utf8");
const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

/** The `--c-bg` triplet declared in the first rule that opens with `selector`. */
function tokenBg(selectorPattern: RegExp): string {
  const rule = selectorPattern.exec(css);
  if (!rule) throw new Error(`no rule matches ${selectorPattern}`);
  const rest = css.slice(rule.index);
  const hit = /--c-bg:\s*(\d+ \d+ \d+);/.exec(rest);
  if (!hit) throw new Error(`no --c-bg after ${selectorPattern}`);
  return hit[1];
}

/** The `rgb(r g b)` literal the boot rule paints, as a triplet. */
function bootBg(selector: string): string {
  const escaped = selector.replace(/[.#]/g, "\\$&");
  const hit = new RegExp(`${escaped}\\s*\\{[^}]*background:\\s*rgb\\((\\d+ \\d+ \\d+)\\)`).exec(html);
  if (!hit) throw new Error(`index.html has no background: rgb() for ${selector}`);
  return hit[1];
}

describe("the boot screen's colours", () => {
  it("paint the dark --c-bg", () => {
    expect(bootBg("#gp-boot")).toBe(tokenBg(/^ {2}:root,\s*\n/m));
  });

  it("paint the light --c-bg", () => {
    expect(bootBg("html.light #gp-boot")).toBe(tokenBg(/^ {2}html\.light \{/m));
  });
});
