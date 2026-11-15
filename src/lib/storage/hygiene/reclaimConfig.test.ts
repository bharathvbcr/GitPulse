import { describe, expect, it } from "vitest";
import { anyScopeOn, cloneBudgetOf, privateBytesTotal, scopeLabel, scopesOf } from "./reclaimConfig";

describe("reclaim config helpers", () => {
  it("reads a missing scopes and clone_budget as the backend defaults", () => {
    expect(scopesOf({})).toEqual({ worktree_caches: false, per_tree_vetoes: false, prune_worktrees: false, worktree_idle_days: 7, agent_scratch_builds: false, agent_scratch_days: 7 });
    expect(cloneBudgetOf({})).toEqual({ max_target_clones: 0, max_private_bytes: 50 * 1024 ** 3 });
  });
  it("keeps what is present and fills only what is missing", () => {
    const s = scopesOf({ scopes: { worktree_caches: true, per_tree_vetoes: false, prune_worktrees: false, worktree_idle_days: 30, agent_scratch_builds: false, agent_scratch_days: 7 } });
    expect(s.worktree_caches).toBe(true);
    expect(s.worktree_idle_days).toBe(30);
    expect(cloneBudgetOf({ clone_budget: { max_target_clones: 0, max_private_bytes: 5 } })).toEqual({ max_target_clones: 0, max_private_bytes: 5 });
  });
  it("does not hand out the shared default object", () => {
    const a = scopesOf({}); a.prune_worktrees = true;
    expect(scopesOf({}).prune_worktrees).toBe(false);
  });
  it("anyScopeOn ignores the idle-days setting", () => {
    expect(anyScopeOn(scopesOf({}))).toBe(false);
    expect(anyScopeOn({ ...scopesOf({}), worktree_idle_days: 90 })).toBe(false);
    expect(anyScopeOn({ ...scopesOf({}), per_tree_vetoes: true })).toBe(true);
    expect(anyScopeOn({ ...scopesOf({}), agent_scratch_days: 90 })).toBe(false);
    expect(anyScopeOn({ ...scopesOf({}), agent_scratch_builds: true })).toBe(true);
  });
  it("sums only the sized worktrees and counts them", () => {
    expect(privateBytesTotal([{ private_bytes: 10 }, {}, { private_bytes: 5 }])).toEqual({ total: 15, sized: 2 });
    expect(privateBytesTotal([])).toEqual({ total: 0, sized: 0 });
  });
  it("labels scopes, falling back to repository", () => {
    expect(scopeLabel("worktree-cache")).toBe("worktree cache");
    expect(scopeLabel("prune-worktree")).toBe("stale worktree");
    expect(scopeLabel("agent-session-scratch")).toBe("agent scratchpad");
    expect(scopeLabel("agent-repo-scratch")).toBe("scratch build");
    expect(scopeLabel("agent-claude-target")).toBe("build folder under .claude");
    expect(scopeLabel("repository")).toBe("repository");
    expect(scopeLabel("")).toBe("repository");
  });
});
