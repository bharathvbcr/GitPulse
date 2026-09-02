import { describe, expect, it } from "vitest";
import { agentKind, agentKindsOn, agentSessionSlug, isAgentWorktree } from "./agentWorktree";

/**
 * Adversarial paths: the detector must terminate, never throw, and never
 * label git internals or human checkouts as agent sessions.
 */
const ADVERSARIAL: readonly string[] = [
  "",
  " ",
  "\0",
  "/",
  "//",
  "\\\\",
  ".".repeat(10_000),
  "/repo/" + "../".repeat(200) + ".claude/worktrees/x",
  "/repo/.git/worktrees/" + "a".repeat(4000),
  "/repo/.git/worktrees/feature",
  "/repo/.GIT/worktrees/feature",
  "/repo/worktrees/feature",
  "/repo/.claude",
  "/repo/.claude/",
  "/repo/.claude/not-worktrees/x",
  "/.claude/worktrees/",
  "claude/worktrees/x",
  "/home/claude/worktrees/x",
  "/repo/.claude/worktrees/" + "slug/".repeat(50) + "file.ts",
  "C:\\repo\\.git\\worktrees\\feature",
  "C:\\repo\\.claude\\worktrees\\slug",
  "/repo/./.claude/worktrees/x",
];

describe("agent worktree detector under adversarial paths", () => {
  it("never throws and never labels git internals as an agent", () => {
    for (const path of ADVERSARIAL) {
      expect(() => isAgentWorktree(path), path).not.toThrow();
      expect(() => agentKind(path), path).not.toThrow();
      expect(() => agentSessionSlug(path), path).not.toThrow();
      if (path.replace(/\\/g, "/").toLowerCase().includes("/.git/worktrees")) {
        expect(isAgentWorktree(path), path).toBe(false);
        expect(agentKind(path), path).toBe("");
      }
    }
  });

  it("stays bounded across hundreds of mixed paths", () => {
    const paths = Array.from({ length: 400 }, (_, i) => {
      if (i % 4 === 0) return `/repo/.claude/worktrees/s${i}`;
      if (i % 4 === 1) return `/repo/.cursor/worktrees/s${i}`;
      if (i % 4 === 2) return `/repo/.git/worktrees/s${i}`;
      return `/repo/wt/${i}`;
    });
    const kinds = agentKindsOn(paths);
    expect(kinds).toEqual(["claude", "cursor"]);
    expect(paths.filter(isAgentWorktree)).toHaveLength(200);
  });
});
