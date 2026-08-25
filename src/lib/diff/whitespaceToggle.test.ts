import { describe, expect, it } from "vitest";
import { decideWhitespaceRefetch } from "./whitespaceToggle";

const statuses = [
  { path: "src/staged.ts", is_staged: true },
  { path: "src/unstaged.ts", is_staged: false },
];

describe("decideWhitespaceRefetch", () => {
  it("keeps a staged selection staged when toggling", () => {
    expect(decideWhitespaceRefetch({ filePath: "src/staged.ts", commitId: null, statuses })).toEqual({
      refetch: true,
      isStaged: true,
    });
  });

  it("keeps an unstaged selection unstaged", () => {
    expect(decideWhitespaceRefetch({ filePath: "src/unstaged.ts", commitId: null, statuses })).toEqual({
      refetch: true,
      isStaged: false,
    });
  });

  it("never swaps a commit diff for the worktree diff", () => {
    expect(
      decideWhitespaceRefetch({ filePath: "src/staged.ts", commitId: "abc123", statuses })
    ).toEqual({ refetch: false, isStaged: false });
  });

  it("skips pseudo-paths from range diffs that have no status entry", () => {
    expect(
      decideWhitespaceRefetch({ filePath: "main...feature", commitId: null, statuses })
    ).toEqual({ refetch: false, isStaged: false });
  });

  it("handles empty selections and empty status lists", () => {
    expect(decideWhitespaceRefetch({ filePath: null, commitId: null, statuses })).toEqual({
      refetch: false,
      isStaged: false,
    });
    expect(
      decideWhitespaceRefetch({ filePath: "src/staged.ts", commitId: null, statuses: [] })
    ).toEqual({ refetch: false, isStaged: false });
  });

  it("prefers the staged entry when a path appears on both sides", () => {
    expect(
      decideWhitespaceRefetch({
        filePath: "dupe.ts",
        commitId: null,
        statuses: [
          { path: "dupe.ts", is_staged: true },
          { path: "dupe.ts", is_staged: false },
        ],
      }).isStaged
    ).toBe(true);
  });
});
