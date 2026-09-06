import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it } from "vitest";
import {
  motionDuration,
  prefersReducedMotion,
  reduceMotionOverrideEnabled,
  setReduceMotionOverride,
  type MediaMatch,
} from "./easing";
import { MOTION_ATTRIBUTE, REDUCED, applyReduceMotion } from "./motionPreference";

const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");

function media(matches: boolean): MediaMatch {
  return { matchMedia: () => ({ matches }) as MediaQueryList };
}

class FakeRoot {
  readonly attributes = new Map<string, string>();
  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }
  removeAttribute(name: string) {
    this.attributes.delete(name);
  }
}

afterEach(() => setReduceMotionOverride(false));

describe("the in-app reduce-motion preference", () => {
  it("adds reduction on top of the system preference", () => {
    applyReduceMotion(true, new FakeRoot());
    expect(reduceMotionOverrideEnabled()).toBe(true);
    expect(prefersReducedMotion(media(false))).toBe(true);
    expect(motionDuration(180, media(false))).toBe(0);
  });

  it("never overrules a system that already asked for less motion", () => {
    // The one-way rule: off means "follow the system", not "animate anyway".
    applyReduceMotion(false, new FakeRoot());
    expect(prefersReducedMotion(media(true))).toBe(true);
    expect(motionDuration(180, media(true))).toBe(0);
  });

  it("returns to following the system when switched back off", () => {
    const root = new FakeRoot();
    applyReduceMotion(true, root);
    applyReduceMotion(false, root);
    expect(prefersReducedMotion(media(false))).toBe(false);
    expect(motionDuration(180, media(false))).toBe(180);
  });

  it("stamps and clears the attribute app.css keys off", () => {
    const root = new FakeRoot();
    applyReduceMotion(true, root);
    expect(root.attributes.get(MOTION_ATTRIBUTE)).toBe(REDUCED);
    applyReduceMotion(false, root);
    expect(root.attributes.has(MOTION_ATTRIBUTE)).toBe(false);
  });

  it("still sets the JS override when there is no document to stamp", () => {
    // Both halves matter: transitions would keep animating if only the CSS
    // half were wired, and the reverse would leave view entrances running.
    applyReduceMotion(true, null);
    expect(prefersReducedMotion(media(false))).toBe(true);
  });
});

describe("app.css mirrors every reduced-motion rule onto the attribute", () => {
  /** The bodies of each `@media (prefers-reduced-motion: reduce)` block. */
  function reduceBlocks(): string[] {
    const blocks: string[] = [];
    const header = "@media (prefers-reduced-motion: reduce)";
    let from = 0;
    for (;;) {
      const start = css.indexOf(header, from);
      if (start < 0) break;
      let depth = 0;
      let end = start;
      for (let i = css.indexOf("{", start); i < css.length; i += 1) {
        if (css[i] === "{") depth += 1;
        if (css[i] === "}") {
          depth -= 1;
          if (depth === 0) {
            end = i;
            break;
          }
        }
      }
      blocks.push(css.slice(css.indexOf("{", start) + 1, end));
      from = end + 1;
    }
    return blocks;
  }

  /** Selectors of every rule inside a block, normalized to one line. */
  function selectors(block: string): string[] {
    const found: string[] = [];
    // Strip comments so a selector mentioned in prose is not collected.
    const clean = block.replace(/\/\*[\s\S]*?\*\//g, "");
    const rule = /([^{}]+)\{[^{}]*\}/g;
    let match: RegExpExecArray | null;
    while ((match = rule.exec(clean)) !== null) {
      found.push(match[1].trim().replace(/\s+/g, " "));
    }
    return found;
  }

  it("finds the media blocks it is meant to mirror", () => {
    // Guards the guard: a rewritten stylesheet that no longer matches the
    // parser above would otherwise report a vacuous pass.
    const blocks = reduceBlocks();
    expect(blocks.length).toBeGreaterThanOrEqual(2);
    expect(blocks.flatMap(selectors).length).toBeGreaterThanOrEqual(4);
  });

  it("pairs every media-gated selector with a [data-motion] twin", () => {
    const flat = css.replace(/\s+/g, " ");
    for (const selector of reduceBlocks().flatMap(selectors)) {
      // `html …` / `html.macos …` become `html[data-motion="reduced"] …`, on
      // every branch of a comma list; the rest of each selector has to be
      // character-identical, which is what keeps the two triggers from
      // drifting apart.
      const mirrored = selector
        .split(",")
        .map((branch) =>
          branch
            .trim()
            .replace(/^html(\.macos)?/, `html$1[${MOTION_ATTRIBUTE}="${REDUCED}"]`),
        )
        .join(", ");
      expect(mirrored, `not anchored on html: ${selector}`).not.toBe(selector);
      expect(flat, `no attribute twin for: ${selector}`).toContain(mirrored);
    }
  });

  it("lets the attribute switch off the entrance animations too", () => {
    // Silencing the reduce rules is not enough on its own: the animations are
    // declared under `(prefers-reduced-motion: no-preference)`, which is live
    // for a reader whose system has no preference set.
    const noPreference = css.slice(css.indexOf("@media (prefers-reduced-motion: no-preference)"));
    expect(noPreference).toContain(`html:not([${MOTION_ATTRIBUTE}="${REDUCED}"]) .gp-view`);
    expect(noPreference).toContain(`html:not([${MOTION_ATTRIBUTE}="${REDUCED}"]) .gp-pop`);
    expect(noPreference).toContain(
      `html:not([${MOTION_ATTRIBUTE}="${REDUCED}"]) .gp-overlay`,
    );
  });
});
