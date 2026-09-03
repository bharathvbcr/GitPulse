import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulseExtensionChurn.svelte", import.meta.url), "utf8");

describe("PulseExtensionChurn component", () => {
  it("renders the backend's extension roll-up rather than re-deriving it", () => {
    expect(source).toContain("PulseExtensionChurn");
    expect(source).not.toContain("split(\".\")");
  });

  it("bounds the rendered list so a polyglot repo cannot blow up the card", () => {
    expect(source).toContain("extensions.slice(0, 12)");
  });
});
