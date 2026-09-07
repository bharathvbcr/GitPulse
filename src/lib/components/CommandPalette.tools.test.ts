import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

describe("CommandPalette optional-tools entry points", () => {
  const source = readFileSync(join(here, "CommandPalette.svelte"), "utf8");

  it("distinguishes cannot-search from zero symbol hits", () => {
    expect(source).toContain("symbolSearchNote");
    expect(source).toContain("not the same as zero matches");
    expect(source).toContain("Set up devmap");
  });

  it("offers optional tools setup from the palette", () => {
    expect(source).toContain("optional_tools_setup");
    expect(source).toContain("openSetupWizard");
  });
});
