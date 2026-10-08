import { describe, expect, it } from "vitest";
import { describeResetPreview } from "./resetPreview";
import type { PreviewCommit, ResetPreview } from "./types";

const oid = (c: string) => c.repeat(40);
const commit = (c: string, summary: string): PreviewCommit => ({
  commit_id: oid(c),
  summary,
  author_name: "Ada",
  timestamp: 1_700_000_000,
});
const base: ResetPreview = {
  branch: "main",
  head: oid("a"),
  target: oid("b"),
  leaving: [],
  leaving_total: 0,
  unreachable_total: 0,
  gaining_total: 0,
};

describe("describeResetPreview", () => {
  it("names every listed commit and counts the ones the cap left out", () => {
    const { title, message } = describeResetPreview(
      {
        ...base,
        leaving: [commit("c", "fix: the thing"), commit("d", "feat: other")],
        leaving_total: 5,
        unreachable_total: 3,
      },
      "HEAD@{4}",
    );
    expect(title).toBe("Reset Branch Here");
    expect(message).toContain('Move branch "main" from aaaaaaa to bbbbbbb (HEAD@{4}).');
    expect(message).toContain("5 commits would leave the branch:");
    expect(message).toContain("ccccccc fix: the thing");
    expect(message).toContain("ddddddd feat: other");
    expect(message).toContain("…and 3 more");
    expect(message).toContain("3 of them are on no other branch, tag or remote");
    expect(message).toContain("git reset --keep");
  });

  it("says nothing is left behind when every leaving commit is reachable elsewhere", () => {
    const { message } = describeResetPreview(
      { ...base, leaving: [commit("c", "x")], leaving_total: 1 },
      "HEAD@{1}",
    );
    expect(message).toContain("1 commit would leave the branch:");
    expect(message).toContain("still reachable from another branch");
    expect(message).not.toContain("…and");
  });

  it("states a pure forward move and a detached HEAD plainly", () => {
    const { title, message } = describeResetPreview(
      { ...base, branch: null, gaining_total: 2 },
      "HEAD@{0}",
    );
    expect(title).toBe("Move Detached HEAD Here");
    expect(message).toContain("Move detached HEAD from");
    expect(message).toContain("No commits leave the branch.");
    expect(message).toContain("would gain 2 commits");
    expect(message).toContain("only HEAD");
  });
});
