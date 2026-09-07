import { describe, expect, it } from "vitest";
import { parseTagList } from "./types";

describe("parseTagList", () => {
  it("unwraps a complete listing", () => {
    const parsed = parseTagList({
      tags: [{ name: "v1", commit_id: "abc", message: "release" }],
      truncated: false,
    });
    expect(parsed.failed).toBe(false);
    expect(parsed.truncated).toBe(false);
    expect(parsed.tags).toEqual([
      {
        name: "v1",
        commit_id: "abc",
        message: "release",
        commits_ahead_of_base: 0,
        commits_behind_base: 0,
        compared_to: null,
      },
    ]);
  });

  it("carries the comparison against the default base", () => {
    const parsed = parseTagList({
      tags: [
        {
          name: "retired/attempt",
          commit_id: "abc",
          message: null,
          commits_ahead_of_base: 16,
          commits_behind_base: 3,
          compared_to: "main",
        },
      ],
      truncated: false,
    });
    expect(parsed.failed).toBe(false);
    expect(parsed.tags[0].commits_ahead_of_base).toBe(16);
    expect(parsed.tags[0].commits_behind_base).toBe(3);
    expect(parsed.tags[0].compared_to).toBe("main");
  });

  it("reads an absent comparison as not-compared, never as zero commits ahead", () => {
    // A payload from a build that did not measure tags. Landing this as
    // `compared_to: null` is what stops the sidebar calling every tag merged.
    const parsed = parseTagList({
      tags: [{ name: "v1", commit_id: "abc" }],
      truncated: false,
    });
    expect(parsed.failed).toBe(false);
    expect(parsed.tags[0].compared_to).toBeNull();
    expect(parsed.tags[0].commits_ahead_of_base).toBe(0);
  });

  it("fails closed when a comparison field is present but the wrong type", () => {
    const bad = (tag: Record<string, unknown>) =>
      parseTagList({ tags: [{ name: "v1", commit_id: "abc", ...tag }], truncated: false }).failed;
    expect(bad({ commits_ahead_of_base: "2" })).toBe(true);
    expect(bad({ commits_behind_base: null })).toBe(true);
    expect(bad({ compared_to: 7 })).toBe(true);
  });

  it("treats a bare array as a failed read, not an empty tag list", () => {
    // The failure this prevents: cmd_list_tags used to return Vec<TagInfo>,
    // and a cap that hid older tags looked like "this repo has exactly 400".
    const parsed = parseTagList([]);
    expect(parsed.failed).toBe(true);
    expect(parsed.tags).toEqual([]);
    expect(parsed.truncated).toBe(false);
  });

  it("fails closed when truncated is missing or the shape is wrong", () => {
    expect(parseTagList({ tags: [] }).failed).toBe(true);
    expect(parseTagList({ tags: [], truncated: "yes" }).failed).toBe(true);
    expect(parseTagList({ truncated: false }).failed).toBe(true);
    expect(parseTagList(null).failed).toBe(true);
    expect(parseTagList({ tags: [{ name: 1, commit_id: "x" }], truncated: false }).failed).toBe(true);
  });

  it("carries the truncated flag through", () => {
    const parsed = parseTagList({
      tags: [{ name: "v9", commit_id: "def" }],
      truncated: true,
    });
    expect(parsed.failed).toBe(false);
    expect(parsed.truncated).toBe(true);
    expect(parsed.tags[0].message).toBeNull();
  });
});
