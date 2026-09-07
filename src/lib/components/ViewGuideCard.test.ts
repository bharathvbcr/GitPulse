import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { destinationGuide } from "../views/viewGuide";

const here = dirname(fileURLToPath(import.meta.url));
const card = readFileSync(join(here, "ViewGuideCard.svelte"), "utf8");
const art = readFileSync(join(here, "ViewGuideArt.svelte"), "utf8");

describe("ViewGuideCard", () => {
  it("compiles", () => {
    const result = compile(card, { filename: "ViewGuideCard.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("paints the catalog summary, shortcut and schematic, not a restated label", () => {
    expect(card).toContain("guide.summary");
    expect(card).toContain("guide.shortcut");
    expect(card).toContain("ViewGuideArt");
    expect(card).toContain("guide.chips");
    const work = destinationGuide("work");
    expect(work?.summary).toContain("worktrees");
  });
});

describe("ViewGuideArt", () => {
  it("compiles", () => {
    const result = compile(art, { filename: "ViewGuideArt.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("keeps a distinct schematic for each view and for the lenses that do not look like the parent", () => {
    expect(art).toContain('view === "work"');
    expect(art).toContain('view === "code"');
    expect(art).toContain('view === "history"');
    expect(art).toContain('view === "insights"');
    expect(art).toContain('section === "resolve"');
    expect(art).toContain('section === "blame"');
    expect(art).toContain('section === "map"');
    expect(art).toContain('section === "diff"');
    expect(art).toContain('section === "coverage"');
  });
});
