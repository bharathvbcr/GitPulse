import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

describe("DiffViewer row chrome", () => {
  it("does not draw a horizontal rule under every diff line", () => {
    expect(source).not.toContain("divide-y");
    expect(source).not.toMatch(/border-y\s+border-border/);
  });
});
