import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./LabelInput.svelte", import.meta.url), "utf8");

describe("LabelInput", () => {
  it("compiles a chip editor that commits on Enter and comma", () => {
    const { warnings } = compile(source, { generate: "client", filename: "LabelInput.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toContain("chip");
    expect(source).toContain('event.key === "Enter"');
    expect(source).toContain('event.key === ","');
  });
});
