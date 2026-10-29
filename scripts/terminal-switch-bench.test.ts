import { describe, expect, it } from "vitest";
import { summarize } from "./terminal-switch-bench.mjs";

const trial = (index: number, arm: "held" | "fresh", firstDrawMs: number) => ({
  kind: "trial", arm, index, syncMs: arm === "fresh" ? 5 : 0, firstDrawMs, frameMs: 50, hiddenState: arm === "held" ? "webgl" : "dom", sameCanvas: arm === "held",
});

describe("terminal switch bench summary", () => {
  it("pairs each held trial with the fresh one beside it in the A B B A order", () => {
    const lines = summarize([trial(0, "held", 17), trial(1, "fresh", 24), trial(2, "fresh", 23), trial(3, "held", 18)]);
    expect(lines).toContain("pairs        fresh slower in 2/2, firstDraw gap min=5.0 med=7.0 max=7.0 ms");
  });

  it("counts a pair where the fresh show was faster, so a gap inside the noise shows as one", () => {
    const lines = summarize([trial(0, "held", 20), trial(1, "fresh", 19), trial(2, "fresh", 22), trial(3, "held", 17)]);
    expect(lines.find((line) => line.startsWith("pairs"))).toContain("fresh slower in 1/2");
  });

  it("reports each arm's order statistics on its own", () => {
    const lines = summarize([trial(0, "held", 17), trial(1, "fresh", 30), trial(2, "fresh", 20), trial(3, "held", 19)]);
    expect(lines).toContain("firstDrawMs  held   n=2 min=17.0 med=19.0 p90=19.0 max=19.0");
    expect(lines).toContain("firstDrawMs  fresh  n=2 min=20.0 med=30.0 p90=30.0 max=30.0");
  });

  it("prints the footprint of one held context", () => {
    const [line] = summarize([{ kind: "memory", dpr: 2, cols: 120, rows: 40, drawingBufferBytes: 11 * 1048576, atlasPages: 1, atlasPageSizes: "512x512", atlasBytes: 1048576, maxTextureSize: 16384, maxAtlasPages: 16 }]);
    expect(line).toBe("footprint    dpr=2 120x40 drawingBuffer=11.0 MiB atlas=1 page(s) 512x512 = 1.0 MiB maxTexture=16384 maxPages=16");
  });
});
