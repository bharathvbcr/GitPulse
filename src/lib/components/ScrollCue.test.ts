import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "ScrollCue.svelte"), "utf8");

describe("ScrollCue", () => {
  it("compiles", () => {
    const result = compile(source, { filename: "ScrollCue.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("advertises overflow with a chevron that is not in the tab order", () => {
    expect(source).toContain("ChevronLeft");
    expect(source).toContain("ChevronRight");
    expect(source).toContain("ChevronUp");
    expect(source).toContain("ChevronDown");
    expect(source).toContain("observeOverflow");
    expect(source).toContain("scrollOverflowBy");
    expect(source).toContain('tabindex="-1"');
    expect(source).toContain("prefersReducedMotion");
    expect(source).toContain('aria-label={label}');
  });

  it("lets clicks through the fade and only captures the chevron", () => {
    expect(source).toContain("gp-edge-fade");
    expect(source).toContain("gp-edge-fade-y");
    expect(source).toContain("gp-scroll-cue-btn");
    expect(source).toContain("e.stopPropagation()");
    expect(source).toContain("e.preventDefault()");
  });
});
