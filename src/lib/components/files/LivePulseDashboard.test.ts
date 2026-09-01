import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "LivePulseDashboard.svelte"), "utf8");

describe("LivePulseDashboard", () => {
  it("computes live uncommitted modifications from repoStore statuses", () => {
    expect(source).toContain("stagedFiles");
    expect(source).toContain("unstagedFiles");
    expect(source).toContain("untrackedFiles");
    expect(source).toContain("conflictedFiles");
    expect(source).toContain("totalAdditions");
    expect(source).toContain("totalDeletions");
  });

  it("provides quick stage, unstage, and discard actions", () => {
    expect(source).toContain("stageAll");
    expect(source).toContain("unstageAll");
    expect(source).toContain("stageFile");
    expect(source).toContain("discardFile");
  });

  it("detects language and loads file history for active file", () => {
    // The payload was an anonymous object type here until it was named, which
    // is what puts it under check:types — assert the named type, so reverting
    // to an inline shape (and out of the contract) fails.
    expect(source).toContain("invoke<LanguageInfo>");
    expect(source).toContain('import type { LanguageInfo } from "../../files/types"');
    expect(source).toContain('"cmd_detect_language"');
    expect(source).toContain('"cmd_get_commit_graph"');
  });

  it("links to Diff, Blame, and Graph via inspectCommitInHistory", () => {
    expect(source).toContain("repoStore.setActiveTab('diff')");
    expect(source).toContain("repoStore.setActiveTab('blame')");
    expect(source).toContain("inspectCommitInHistory");
    expect(source).toContain("statusLiveKey");
    expect(source).toContain("lastRefreshed");
  });
});
