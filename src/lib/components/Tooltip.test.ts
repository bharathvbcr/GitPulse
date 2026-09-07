import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "Tooltip.svelte"),
  "utf8",
);

describe("Tooltip canvas hover", () => {
  it("resolves anchors through tooltipAnchorFromTarget so a canvas does not inherit a gutter title", () => {
    expect(source).toContain("tooltipAnchorFromTarget");
    expect(source).toContain("TOOLTIP_ANCHOR_SELECTOR");
    expect(source).not.toContain('target.closest<HTMLElement>("[title], [data-tip-text]")');
    expect(source).not.toContain('related.closest("[title], [data-tip-text]")');
  });
});

describe("Tooltip destination guides", () => {
  it("renders ViewGuideCard when the anchor carries data-tip-guide", () => {
    expect(source).toContain("destinationGuide");
    expect(source).toContain("ViewGuideCard");
    expect(source).toContain("tipGuideOf");
  });
});
