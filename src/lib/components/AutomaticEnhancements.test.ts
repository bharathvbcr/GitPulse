import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./AutomaticEnhancements.svelte", import.meta.url), "utf8");

describe("AutomaticEnhancements", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "AutomaticEnhancements.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("can render as a compact header chip instead of a permanent full-width bar", () => {
    expect(source).toContain("compact = false");
    expect(source).toContain('data-size={compact ? "sm" : "md"}');
    expect(source).not.toContain("gp-pill");
    expect(source).toContain('compact ? "Auto"');
  });
});
