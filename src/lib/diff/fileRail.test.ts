import { describe, expect, it } from "vitest";
import {
  EMPTY_RAIL,
  buildFileRail,
  churnLabel,
  displayName,
  entryKey,
  isCurrent,
  railPosition,
  stepFile,
  truncationNote,
  type RailInput,
} from "./fileRail";

function input(over: Partial<RailInput> = {}): RailInput {
  return {
    selectionKind: "commit",
    commitFiles: [],
    commitFilesTruncated: false,
    commitFilesTotal: 0,
    statuses: [],
    ...over,
  };
}

const commitFile = (path: string, additions = 1, deletions = 0) => ({
  path,
  status_code: "M",
  additions,
  deletions,
});

const worktreeFile = (path: string, is_staged = false, over = {}) => ({
  path,
  status_code: "M",
  is_staged,
  additions: 1,
  deletions: 0,
  ...over,
});

describe("buildFileRail", () => {
  it("lists a commit's files so a second file is one click away", () => {
    const rail = buildFileRail(
      input({ commitFiles: [commitFile("src/a.ts"), commitFile("src/b.ts")] }),
    );
    expect(rail.source).toBe("commit");
    expect(rail.entries.map((e) => e.path)).toEqual(["src/a.ts", "src/b.ts"]);
  });

  it("lists working-tree files, keeping the staged split", () => {
    const rail = buildFileRail(
      input({
        selectionKind: "file",
        statuses: [worktreeFile("a.ts", true), worktreeFile("a.ts", false)],
      }),
    );
    expect(rail.source).toBe("worktree");
    expect(rail.entries.map((e) => e.isStaged)).toEqual([true, false]);
  });

  it("renders nothing for a range diff rather than inventing a list", () => {
    // The backend returns one combined patch for `from..to`; parsing file
    // names out of the diff text would produce a list that silently disagrees
    // with what is on screen.
    expect(buildFileRail(input({ selectionKind: "range" }))).toEqual(EMPTY_RAIL);
  });

  it("tells a commit whose details have not landed from one that changed nothing", () => {
    // Null is "not loaded"; an empty array is "loaded, and empty". Rendering
    // an empty rail for the first would claim the commit touched no files.
    expect(buildFileRail(input({ commitFiles: null })).source).toBe("none");
    expect(buildFileRail(input({ commitFiles: [] })).source).toBe("none");
  });

  it("carries a truncated list's truncation, never presenting it as whole", () => {
    const rail = buildFileRail(
      input({
        commitFiles: [commitFile("a"), commitFile("b")],
        commitFilesTruncated: true,
        commitFilesTotal: 312,
      }),
    );
    expect(rail.truncated).toBe(true);
    expect(rail.totalCount).toBe(312);
    expect(truncationNote(rail)).toBe("showing 2 of 312 files");
  });

  it("still says it is truncated when the true total is unknown", () => {
    const rail = buildFileRail(
      input({ commitFiles: [commitFile("a")], commitFilesTruncated: true, commitFilesTotal: 0 }),
    );
    expect(truncationNote(rail)).toBe("showing the first 1 files");
  });

  it("adds no note when the list is whole", () => {
    expect(truncationNote(buildFileRail(input({ commitFiles: [commitFile("a")] })))).toBe("");
  });

  it("ignores a nonsensical total rather than rendering 'showing 5 of 2'", () => {
    const rail = buildFileRail(
      input({
        commitFiles: [commitFile("a"), commitFile("b")],
        commitFilesTruncated: true,
        commitFilesTotal: 1,
      }),
    );
    expect(rail.totalCount).toBe(0);
    expect(truncationNote(rail)).toBe("showing the first 2 files");
  });
});

describe("entryKey", () => {
  it("separates the staged and unstaged sides of one path", () => {
    // Staging part of a file leaves the same path on both sides with
    // different content; keying on path alone highlights the wrong row.
    expect(entryKey({ path: "a.ts", isStaged: true })).not.toBe(
      entryKey({ path: "a.ts", isStaged: false }),
    );
  });
});

describe("isCurrent", () => {
  const entry = { path: "a.ts", statusCode: "M", additions: 0, deletions: 0, isStaged: false };

  it("matches on path alone for a commit rail", () => {
    expect(isCurrent(entry, "a.ts", true, "commit")).toBe(true);
  });

  it("requires the staged side to match on a worktree rail", () => {
    expect(isCurrent(entry, "a.ts", false, "worktree")).toBe(true);
    expect(isCurrent(entry, "a.ts", true, "worktree")).toBe(false);
  });

  it("matches nothing when no file is selected", () => {
    expect(isCurrent(entry, null, false, "worktree")).toBe(false);
  });
});

describe("stepFile", () => {
  const rail = buildFileRail(
    input({ commitFiles: [commitFile("a"), commitFile("b"), commitFile("c")] }),
  );

  it("walks forward and back through the list", () => {
    expect(stepFile(rail, "a", false, 1)?.path).toBe("b");
    expect(stepFile(rail, "b", false, -1)?.path).toBe("a");
  });

  it("stops at both edges instead of wrapping", () => {
    // Wrapping sends the reader back to the first file with nothing on screen
    // saying so, which reads as a broken button rather than an end of list.
    expect(stepFile(rail, "c", false, 1)).toBeNull();
    expect(stepFile(rail, "a", false, -1)).toBeNull();
  });

  it("opens a sensible file from a cold start", () => {
    expect(stepFile(rail, null, false, 1)?.path).toBe("a");
    expect(stepFile(rail, null, false, -1)?.path).toBe("c");
  });

  it("returns nothing for an empty rail or a zero step", () => {
    expect(stepFile(EMPTY_RAIL, "a", false, 1)).toBeNull();
    expect(stepFile(rail, "a", false, 0)).toBeNull();
  });

  it("steps between the two sides of one staged path", () => {
    const split = buildFileRail(
      input({
        selectionKind: "file",
        statuses: [worktreeFile("a.ts", true), worktreeFile("a.ts", false)],
      }),
    );
    expect(stepFile(split, "a.ts", true, 1)?.isStaged).toBe(false);
    expect(stepFile(split, "a.ts", false, -1)?.isStaged).toBe(true);
  });
});

describe("railPosition", () => {
  const rail = buildFileRail(
    input({ commitFiles: [commitFile("a"), commitFile("b"), commitFile("c")] }),
  );

  it("reports a 1-based position for the readout", () => {
    expect(railPosition(rail, "b", false)).toEqual({ index: 2, total: 3 });
  });

  it("reports index 0 when the open file is not in the list", () => {
    expect(railPosition(rail, "zzz", false)).toEqual({ index: 0, total: 3 });
  });
});

describe("labels", () => {
  it("shows churn only when there is any", () => {
    expect(churnLabel({ path: "a", statusCode: "M", additions: 12, deletions: 3, isStaged: false }))
      .toBe("+12 −3");
    expect(churnLabel({ path: "a", statusCode: "A", additions: 0, deletions: 0, isStaged: false }))
      .toBe("");
  });

  it("names a file by its basename", () => {
    expect(
      displayName({ path: "src/lib/a.ts", statusCode: "M", additions: 0, deletions: 0, isStaged: false }),
    ).toBe("a.ts");
  });

  it("shows a rename as old → new", () => {
    expect(
      displayName({
        path: "src/b.ts",
        oldPath: "src/a.ts",
        statusCode: "R",
        additions: 0,
        deletions: 0,
        isStaged: false,
      }),
    ).toBe("a.ts → b.ts");
  });

  it("does not render an arrow when a rename kept the name", () => {
    expect(
      displayName({
        path: "src/a.ts",
        oldPath: "src/a.ts",
        statusCode: "R",
        additions: 0,
        deletions: 0,
        isStaged: false,
      }),
    ).toBe("a.ts");
  });

  it("survives a path with no directory and a trailing slash", () => {
    const base = (path: string) =>
      displayName({ path, statusCode: "M", additions: 0, deletions: 0, isStaged: false });
    expect(base("a.ts")).toBe("a.ts");
    expect(base("src/dir/")).toBe("dir");
  });
});
