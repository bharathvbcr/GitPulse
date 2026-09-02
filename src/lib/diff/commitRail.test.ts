import { describe, expect, it } from "vitest";
import {
  EMPTY_COMMIT_RAIL,
  MAX_PICKER_COMMITS,
  buildCommitRail,
  commitLabel,
  isCurrentCommit,
  pickerNote,
  type CommitRowLike,
} from "./commitRail";

const row = (id: string, over: Partial<CommitRowLike> = {}): CommitRowLike => ({
  id,
  summary: `summary ${id}`,
  author_name: "Ada",
  timestamp: 1_700_000_000,
  is_merge: false,
  ...over,
});

describe("buildCommitRail", () => {
  it("carries the fields the picker renders", () => {
    const rail = buildCommitRail([row("a1b2c3d", { summary: "Fix the parser", is_merge: true })]);
    expect(rail.entries[0]).toMatchObject({
      id: "a1b2c3d",
      summary: "Fix the parser",
      authorName: "Ada",
      isMerge: true,
    });
  });

  it("preserves the graph's own order rather than re-sorting", () => {
    // Re-sorting would make the picker disagree with the graph beside it, and
    // the reader has no way to tell which one is lying.
    const rail = buildCommitRail([row("c", { timestamp: 1 }), row("a", { timestamp: 9 })]);
    expect(rail.entries.map((e) => e.id)).toEqual(["c", "a"]);
  });

  it("is empty for a graph that has not loaded", () => {
    expect(buildCommitRail(null)).toEqual(EMPTY_COMMIT_RAIL);
    expect(buildCommitRail(undefined)).toEqual(EMPTY_COMMIT_RAIL);
    expect(buildCommitRail([])).toEqual(EMPTY_COMMIT_RAIL);
  });

  it("caps a long history and says so", () => {
    const rows = Array.from({ length: 500 }, (_, i) => row(`c${i}`));
    const rail = buildCommitRail(rows);
    expect(rail.entries).toHaveLength(MAX_PICKER_COMMITS);
    expect(rail.truncated).toBe(true);
    expect(rail.totalCount).toBe(500);
    expect(pickerNote(rail)).toContain(`of 500`);
    // The note routes somewhere, rather than only apologising.
    expect(pickerNote(rail)).toContain("Graph");
  });

  it("adds no note when the whole history fits", () => {
    const rail = buildCommitRail([row("a"), row("b")]);
    expect(rail.truncated).toBe(false);
    expect(pickerNote(rail)).toBe("");
  });

  it("treats a zero or negative limit as showing nothing, not everything", () => {
    const rows = [row("a"), row("b")];
    expect(buildCommitRail(rows, 0).entries).toHaveLength(0);
    expect(buildCommitRail(rows, -5).entries).toHaveLength(0);
    // And it still reports that commits exist, rather than reading as empty.
    expect(buildCommitRail(rows, 0).truncated).toBe(true);
  });
});

describe("commitLabel", () => {
  it("uses the summary", () => {
    expect(commitLabel({ id: "a", summary: "Add parser", authorName: "", timestamp: 0, isMerge: false }))
      .toBe("Add parser");
  });

  it("names an empty message rather than rendering a blank row", () => {
    // `git commit --allow-empty-message` produces these; a blank row looks
    // like a loading failure.
    for (const summary of ["", "   ", "\n"]) {
      expect(
        commitLabel({ id: "a", summary, authorName: "", timestamp: 0, isMerge: false }),
      ).toBe("(no commit message)");
    }
  });
});

describe("isCurrentCommit", () => {
  const entry = { id: "a1b2c3d4e5f6", summary: "", authorName: "", timestamp: 0, isMerge: false };

  it("matches an identical id", () => {
    expect(isCurrentCommit(entry, "a1b2c3d4e5f6")).toBe(true);
  });

  it("matches an abbreviated id against a full one, in either direction", () => {
    // The selection and the graph row can carry different id lengths; a strict
    // equality check would leave the list with nothing highlighted.
    expect(isCurrentCommit(entry, "a1b2c3d")).toBe(true);
    expect(
      isCurrentCommit({ ...entry, id: "a1b2c3d" }, "a1b2c3d4e5f6"),
    ).toBe(true);
  });

  it("refuses a prefix too short to mean anything", () => {
    expect(isCurrentCommit(entry, "a1b")).toBe(false);
    expect(isCurrentCommit(entry, "a")).toBe(false);
  });

  it("does not match a different commit", () => {
    expect(isCurrentCommit(entry, "ffffffffffff")).toBe(false);
    expect(isCurrentCommit(entry, null)).toBe(false);
    expect(isCurrentCommit(entry, "")).toBe(false);
  });
});
