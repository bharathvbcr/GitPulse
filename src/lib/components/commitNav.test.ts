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
    expect(inspect).toContain("repoStore.selectHistoryCommit(entry.commit_id");
    // Pinned: a filter typed above the list must not move the details pane
    // off the entry the reader clicked.
    expect(inspect).toContain("pin: true");
    expect(inspect).not.toContain("inspectCommitInHistory(");
    expect(read("ReflogViewer.svelte")).not.toContain('setActiveTab("diff")');
  });

  it("selects through one path from every Graph list", () => {
    // Commits, HEAD moves and Suspects each wrote the graph selection and the
    // diff selection as a pair of their own, and Suspects left for Diff
    // instead. One path keeps the header, files and diff on the same commit
    // whichever list the click came from.
    const table = read("CommitTable.svelte");
    const suspects = read("SuspectsList.svelte");
    const reflog = read("ReflogViewer.svelte");
    for (const [name, source] of [["CommitTable", table], ["SuspectsList", suspects], ["ReflogViewer", reflog]] as const) {
      expect(source, name).toContain("repoStore.selectHistoryCommit(");
      expect(source, name).not.toContain("graphStore.selectCommit(");
      expect(source, name).not.toContain("graphStore.selectCommitById(");
      expect(source, name).not.toContain("repoStore.selectCommitDiff(");
    }
    // The lists that are not the graph pin; the graph's own clicks do not, so
    // a filter that drops a clicked graph row still falls back as before.
    expect(suspects).toContain("pin: true");
    expect(table).not.toContain("pin: true");
    expect(suspects).not.toContain('setViewSection("history", "diff")');
  });
});
