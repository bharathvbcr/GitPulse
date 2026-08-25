import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "Sidebar.svelte"),
  "utf8"
);

describe("Sidebar batch staging", () => {
  it("offers stage all and unstage all in the change-list headers", () => {
    expect(source).toContain("stageAll");
    expect(source).toContain("unstageAll");
    expect(source).toContain("stage all");
    expect(source).toContain("unstage all");
  });
});
