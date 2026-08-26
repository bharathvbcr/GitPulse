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
    expect(source).not.toContain('target.closest<HTMLElement>("[title], [data-tip-text]")');
  });
});
