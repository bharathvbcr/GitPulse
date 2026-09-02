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
    expect(parsed.tags).toEqual([{ name: "v1", commit_id: "abc", message: "release" }]);
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
