import { describe, expect, it } from "vitest";
import { humanBytes } from "./format";
import { worktreeCountText, worktreeSizeText } from "./worktreeSize";
import type { WorktreeUsage } from "./types";

const GiB = 1024 ** 3;
const wt = (over: Partial<WorktreeUsage> = {}): WorktreeUsage => ({
  path: "/r/.claude/worktrees/agent-a", name: "agent-a", branch: "agent-a",
  bytes: 61.9 * GiB, truncated: false, files_counted: 90_000, ...over,
});

describe("worktreeSizeText", () => {
  it("leads with what removal frees when the private size was read", () => {
    expect(worktreeSizeText(wt({ private_bytes: 2 * GiB }))).toBe(`${humanBytes(2 * GiB)} freed by removal · ${humanBytes(61.9 * GiB)} logical`);
  });
  it("states only the logical size when the private size was not read", () => {
    expect(worktreeSizeText(wt())).toBe(`${humanBytes(61.9 * GiB)} logical`);
  });
  it("carries the floor and how far a truncated walk got", () => {
    expect(worktreeSizeText(wt({ truncated: true, files_counted: 100_000 }))).toBe(
      `≥ ${humanBytes(61.9 * GiB)} logical · first ${(100_000).toLocaleString()} files`,
    );
  });
});

describe("worktreeCountText", () => {
  it("names the sizing cap instead of implying the sized list is all of them", () => {
    expect(worktreeCountText({ worktrees: [wt(), wt()], worktrees_listed: 21 })).toBe("2 sized of 21");
    expect(worktreeCountText({ worktrees: [wt()], worktrees_listed: 1 })).toBe("1");
  });
});
