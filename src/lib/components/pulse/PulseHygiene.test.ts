import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseHygiene.svelte", import.meta.url), "utf8");

describe("PulseHygiene component", () => {
  it("delegates every hygiene number to the tested metric", () => {
    expect(source).toContain("computeHygiene");
  });

  it("does not re-implement conventional-commit matching in the view", () => {
    // The grammar has exactly one frontend owner (pulse/metrics.ts), which is
    // itself contract-tested against analyzer/conventional.rs.
    expect(source).not.toContain("feat|fix");
    expect(source).not.toContain("CONVENTIONAL");
  });
});
