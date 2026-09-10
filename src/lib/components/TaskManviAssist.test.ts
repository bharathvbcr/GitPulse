import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskManviAssist.svelte", import.meta.url), "utf8");

describe("TaskManviAssist", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskManviAssist.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("captures notes, asks Manvi for title and description, and accepts field by field", () => {
    expect(source).toContain("What do you need?");
    expect(source).toContain("Draft with Manvi");
    expect(source).toContain("Improve with Manvi");
    expect(source).toContain("startQuickEnhance");
    expect(source).toContain("acceptEnhancementInput");
    expect(source).toContain("Use this title");
    expect(source).toContain("Use this description");
    expect(source).toContain("Use both");
    expect(source).toContain("bind:value={title}");
    expect(source).toContain("bind:value={description}");
    expect(source).toContain("Type a few sentences");
    expect(source).toContain('aria-label="Fields Manvi may change"');
    expect(source).not.toMatch(/\bbackdrop-blur-/);
  });
});
