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

  it("says how many extensions the bounded list leaves out", () => {
    // A cut list must carry both numbers, never read as the whole repository.
    expect(source).toContain("extensions.length > shown.length");
    expect(source).toContain("Top {shown.length} of {extensions.length}");
  });

  it("colours additions and deletions with status roles and keeps the sign in the text", () => {
    expect(source).toContain('class="text-ok">+');
    expect(source).toContain('class="text-danger">-');
  });
});
