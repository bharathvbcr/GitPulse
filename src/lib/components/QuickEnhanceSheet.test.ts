import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./QuickEnhanceSheet.svelte", import.meta.url), "utf8");

describe("QuickEnhanceSheet", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "QuickEnhanceSheet.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("is a single-blur sheet that surfaces hidden details then Manvi", () => {
    expect(source).toContain("gp-scrim");
    expect(source).toContain("shadow-float");
    expect(source).toContain("hiddenTaskDetails");
    expect(source).toContain("TaskManviAssist");
    expect(source).toContain("quick");
    expect(source).toContain("quick-enhance-title");
    expect(source).toContain("aria-labelledby=\"quick-enhance-title\"");
    expect(source).not.toMatch(/\bbackdrop-blur-/);
    expect(source).toContain("onBusy=");
    expect(source).toContain("Open full editor");
    expect(source).toContain("SettingToggle");
    expect(source).toContain("visibleHiddenDetails");
    expect(source).toContain("Show empty fields");
  });
});
