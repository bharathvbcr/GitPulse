import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { functionBody } from "../stores/sourceScan";

function read(name: string): string {
  return readFileSync(join(dirname(fileURLToPath(import.meta.url)), name), "utf8");
}

describe("blame and reflog commit navigation", () => {
  it("sends blame and pulse SHA clicks to the history tab via inspectCommitInHistory", () => {
    expect(read("BlameViewer.svelte")).toContain("inspectCommitInHistory");
    expect(read("files/LivePulseDashboard.svelte")).toContain("inspectCommitInHistory");
    expect(read("BlameViewer.svelte")).not.toContain('setActiveTab("diff")');
  });

  it("selects a reflog entry's commit in place, by id, under the list", () => {
    // The reflog is drawn inside History › Graph, above the details pane, so
    // a click selects there. Handing off to inspectCommitInHistory is what
    // made the click invisible while Reflog was its own section, and it can
    // only select commits the graph draws; a reflog's point is the ones it
    // does not.
    const inspect = functionBody(read("ReflogViewer.svelte"), "inspectEntry");
    expect(inspect).toContain("graphStore.selectCommitById(entry.commit_id");
    expect(inspect).toContain("repoStore.selectCommitDiff(entry.commit_id)");
    expect(inspect).not.toContain("inspectCommitInHistory(");
    expect(read("ReflogViewer.svelte")).not.toContain('setActiveTab("diff")');
  });
});
