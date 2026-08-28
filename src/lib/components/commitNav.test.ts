import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

function read(name: string): string {
  return readFileSync(join(dirname(fileURLToPath(import.meta.url)), name), "utf8");
}

describe("blame and reflog commit navigation", () => {
  it("sends SHA clicks to the history tab via inspectCommitInHistory", () => {
    expect(read("BlameViewer.svelte")).toContain("inspectCommitInHistory");
    expect(read("ReflogViewer.svelte")).toContain("inspectCommitInHistory");
    expect(read("files/LivePulseDashboard.svelte")).toContain("inspectCommitInHistory");
    expect(read("BlameViewer.svelte")).not.toContain('setActiveTab("diff")');
    expect(read("ReflogViewer.svelte")).not.toContain('setActiveTab("diff")');
  });
});
