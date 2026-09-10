import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./SettingToggle.svelte", import.meta.url), "utf8");

describe("SettingToggle", () => {
  it("compiles with a clickable label and optional disabled", () => {
    const { warnings } = compile(source, { generate: "client", filename: "SettingToggle.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toContain("<label");
    expect(source).toContain("for={switchId}");
    expect(source).toContain("disabled = false");
    expect(source).toContain("{disabled}");
  });
});
