import { describe, expect, it } from "vitest";
import type { FileStatus } from "../stores/repoStore";
import { indexSelectionPaths } from "./bulkOps";

const file = (path: string, status_code = "M ", old_path?: string): FileStatus => ({
  path, status_code, old_path, is_staged: true, is_conflicted: false, additions: 0, deletions: 0,
});

describe("index selection paths", () => {
  it("retains empty and exact single-file selections", () => {
    expect(indexSelectionPaths([], "stage")).toEqual([]);
    expect(indexSelectionPaths([file("literal[1].txt")], "stage")).toEqual(["literal[1].txt"]);
  });
  it("unstages both sides of a rename and deduplicates shared paths", () => {
    expect(indexSelectionPaths([file("new", "R ", "old"), file("old"), file("new")], "unstage")).toEqual(["new", "old"]);
  });
  it("does not unstage the source of a copy", () => {
    expect(indexSelectionPaths([file("copy", "C ", "original")], "unstage")).toEqual(["copy"]);
  });
  it("leaves staged rename sources alone when adding later working edits", () => {
    expect(indexSelectionPaths([file("new", "RM", "old")], "stage")).toEqual(["new"]);
  });
  it("does not invent a source for a malformed or legacy rename row", () => {
    expect(indexSelectionPaths([file("new", "R ")], "unstage")).toEqual(["new"]);
  });
});
