import { describe, expect, it } from "vitest";
import { AGENT_WORKTREE_SEGMENT, agentSessionSlug, isAgentWorktree } from "./agentWorktree";

describe("isAgentWorktree", () => {
  it("recognises the layout Claude Code actually creates", () => {
    expect(isAgentWorktree("/repo/.claude/worktrees/add-parser-8540d4")).toBe(true);
    expect(isAgentWorktree("/Users/me/Code/app/.claude/worktrees/x")).toBe(true);
  });

  it("recognises it on Windows separators too", () => {
    // The same repository opened on Windows reports backslashes; matching only
    // the POSIX form would label every agent worktree there as hand-made.
    expect(isAgentWorktree("C:\\Users\\me\\app\\.claude\\worktrees\\slug")).toBe(true);
  });

  it("does not claim an ordinary worktree", () => {
    expect(isAgentWorktree("/repo")).toBe(false);
    expect(isAgentWorktree("/repo/../wt/feature")).toBe(false);
    expect(isAgentWorktree("/repo/.git/worktrees/feature")).toBe(false);
  });

  it("never guesses from a branch name", () => {
    // A person can name a branch `claude/anything`. Calling their checkout an
    // agent session because of it puts a wrong label on real work, and the
    // label drives which remedy the UI suggests.
    expect(isAgentWorktree("/repo/wt/claude/my-own-branch")).toBe(false);
    expect(isAgentWorktree("/home/claude/projects/app")).toBe(false);
  });

  it("handles empty and malformed input without throwing", () => {
    expect(isAgentWorktree("")).toBe(false);
    expect(isAgentWorktree("   ")).toBe(false);
    expect(isAgentWorktree(AGENT_WORKTREE_SEGMENT)).toBe(true);
  });
});

describe("agentSessionSlug", () => {
  it("returns the whole session segment, hash included", () => {
    // The trailing hash is what distinguishes two sessions working the same
    // feature; trimming to a prettier prefix merges them in the reader's eye.
    expect(agentSessionSlug("/repo/.claude/worktrees/agentic-git-repo-8540d4")).toBe(
      "agentic-git-repo-8540d4",
    );
  });

  it("stops at the session directory, ignoring anything nested below", () => {
    expect(agentSessionSlug("/repo/.claude/worktrees/slug/src/lib/x.ts")).toBe("slug");
  });

  it("works on Windows separators", () => {
    expect(agentSessionSlug("C:\\app\\.claude\\worktrees\\slug\\src")).toBe("slug");
  });

  it("is empty for anything that is not an agent worktree", () => {
    expect(agentSessionSlug("/repo")).toBe("");
    expect(agentSessionSlug("")).toBe("");
  });

  it("is empty when the segment is present but names no session", () => {
    expect(agentSessionSlug("/repo/.claude/worktrees/")).toBe("");
  });
});
