import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskEnhancements.svelte", import.meta.url), "utf8");

describe("TaskEnhancements", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskEnhancements.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("confirms uncertain recovery through the in-app dialog", () => {
    expect(source).toContain("askConfirm");
    expect(source).not.toContain("window.confirm");
    expect(source).toContain("aria-label=\"Manvi task enhancements\"");
  });
});
