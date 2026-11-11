import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./ProductTour.svelte", import.meta.url), "utf8");

/**
 * The tour card used to be the scroller itself, so a short window sliced the
 * body text, or a button, in half at the card's edge. The card is now fixed
 * (header, progress and footer stay in view) and only the body scrolls, with
 * the shared edge fade and scroll padding. The fade masks the element it is on,
 * so it must not sit on the card, whose own surface would fade with it.
 */
describe("ProductTour scroll edge", () => {
  it("scrolls the body, not the card, and fades the body's clipped edge", () => {
    expect(source).toContain('import { scrollEdgeFade } from "../../ui/scrollEdgeFade";');
    expect(source).toMatch(/<div class="[^"]*min-h-0 overflow-y-auto scroll-py-8[^"]*" data-dialog-body use:scrollEdgeFade>/);
    expect(source).toMatch(/\.tour-card \{[^}]*overflow: hidden;/);
    expect(source).not.toMatch(/\.tour-card \{[^}]*overflow: auto;/);
  });

  it("keeps the heading on the type scale and names surfaces as proper nouns", () => {
    expect(source).toMatch(/<h1 id="product-tour-title"[^>]*class="text-lg /);
    expect(source).not.toMatch(/\btext-xl\b/);
    expect(source).toContain(">Open Tasks</button>");
    expect(source).toContain(">Open Settings</button>");
  });
});
