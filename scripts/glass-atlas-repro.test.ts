import { describe, expect, it } from "vitest";
import { summarize } from "./glass-atlas-repro.mjs";

const row = (arm: string, seed: number, twinPixels: number, afterAtlasPixels = 0, afterRefreshPixels = twinPixels) => ({
  arm, seed, dpr: 2, sig: "1/2", blankPixels: 0, twinPixels, twinRows: twinPixels ? 1 : 0, twinRepeatPixels: 0,
  afterRefreshPixels, afterAtlasPixels, webgl: true,
});

describe("glass atlas repro summary", () => {
  it("counts a run as corrupt only when it differs from the single-frame render", () => {
    const [line] = summarize([row("glass-a0", 1, 0), row("glass-a0", 2, 900), row("glass-a0", 3, 40)]);
    expect(line).toContain("corrupt=2/3");
    expect(line).toContain("px[min/med/max]=0/40/900");
  });

  it("says what heals a corrupt run: a fresh atlas, not a row refresh", () => {
    const [line] = summarize([row("glass-a0", 1, 500, 0, 500), row("glass-a0", 2, 700, 0, 700)]);
    expect(line).toContain("atlasHeals=2/2");
    expect(line).toContain("refreshHeals=0/2");
  });

  it("keeps arms apart and reports a clean arm as 0 of n, not as missing", () => {
    const lines = summarize([row("opaque", 1, 0), row("opaque", 2, 0), row("glass-a0", 1, 12)]);
    expect(lines).toHaveLength(2);
    expect(lines[0]).toContain("corrupt=0/2");
    expect(lines[1]).toContain("corrupt=1/1");
  });
});
