import { describe, expect, it } from "vitest";
import { branchTooltip, tagTooltip } from "./branchTooltip";
import type { BranchInfo, TagInfo } from "./types";

function branch(overrides: Partial<BranchInfo> = {}): BranchInfo {
  return {
    name: "feature/avatars",
    is_current: false,
    is_remote: false,
    remote_name: null,
    tip_commit_id: "abc123",
    ahead_count: 0,
    behind_count: 0,
    upstream: "origin/feature/avatars",
    is_default: false,
    is_gone: false,
    last_commit_timestamp: 1_700_000_000,
    last_author: "Ada",
    last_summary: "feat: draw avatars in gutter",
    commits_ahead_of_base: 3,
    commits_behind_base: 0,
    additions: 12,
    deletions: 4,
    files_changed: 2,
    compared_to: "main",
    ...overrides,
  };
}

describe("branchTooltip", () => {
  it("includes name, summary, author, age and upstream metadata", () => {
    const text = branchTooltip(branch(), 1_700_086_400);
    const lines = text.split("\n");
    expect(lines[0]).toBe("feature/avatars");
    expect(lines[1]).toBe("feat: draw avatars in gutter");
    expect(lines[2]).toContain("Ada");
    expect(lines[2]).toMatch(/d ago$/);
    expect(text).toContain("+3 vs base");
    expect(text).toContain("tracks origin/feature/avatars");
  });

  it("flags checked-out, default and gone branches", () => {
    const text = branchTooltip(
      branch({ is_current: true, is_default: true, is_gone: true }),
      undefined,
    );
    expect(text).toContain("checked out");
    expect(text).toContain("default");
    expect(text).toContain("upstream gone");
  });

  it("reports ahead/behind counts when present", () => {
    const text = branchTooltip(branch({ ahead_count: 2, behind_count: 5 }), undefined);
    expect(text).toContain("ahead 2, behind 5");
  });

  it("omits sections whose data is missing instead of printing empty lines", () => {
    const text = branchTooltip(
      branch({
        last_summary: "",
        last_author: "",
        last_commit_timestamp: 0,
        upstream: null,
        commits_ahead_of_base: 0,
      }),
      undefined,
    );
    expect(text.split("\n")).toEqual(["feature/avatars"]);
  });

  it("never emits blank or whitespace-only lines for adversarial data", () => {
    const cases: Array<Partial<BranchInfo>> = [
      { last_summary: "\n\n" },
      { last_author: "   " },
      { name: "" },
      { upstream: "", remote_name: "" },
    ];
    for (const overrides of cases) {
      const text = branchTooltip(branch(overrides), undefined);
      for (const line of text.split("\n")) {
        expect(line.trim().length > 0 || line === "").toBe(true);
      }
      // No double newlines from empty segments.
      expect(text.includes("\n\n")).toBe(false);
    }
  });
});

describe("tagTooltip", () => {
  it("includes name, message and a short commit id", () => {
    const tag: TagInfo = { name: "v1.2.3", commit_id: "abcdef1234567890", message: "Release" };
    const text = tagTooltip(tag);
    expect(text.split("\n")).toEqual(["Tag v1.2.3", "Release", "abcdef123456"]);
  });

  it("handles sparse tags without message or id", () => {
    expect(tagTooltip({ name: "t", commit_id: "" })).toBe("Tag t");
    expect(tagTooltip({ name: "t", commit_id: "x", message: null })).toBe("Tag t\nx");
  });
});
