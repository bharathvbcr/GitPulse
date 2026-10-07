import { describe, expect, it } from "vitest";
import { scopeToRepository } from "./plane";
import { listing, probe, project, snapshot, task, tasks, worktree } from "./__tests__/fixtures";

/**
 * Fleet's agent count opens the Agents view for one repository. The scope is
 * the same identity rule the tab strip stacks on, so naming the repository by
 * any of its checkouts, in any spelling the volume treats as the same, finds
 * the same rows — and nothing from a neighbour whose path merely starts alike.
 */
function plane() {
  return project({
    probes: [
      probe({
        path: "/code/api",
        snapshot: snapshot({
          repo_path: "/code/api",
          worktrees: listing([
            worktree({ path: "/code/api", name: "api", agent_kind: "", session_slug: "", is_main: true }),
            worktree({ path: "/code/api/.claude/worktrees/a", agent_kind: "claude", session_slug: "a", name: "a" }),
          ]),
        }),
      }),
      probe({
        path: "/code/api-v2",
        snapshot: snapshot({
          repo_path: "/code/api-v2",
          worktrees: listing([
            worktree({ path: "/code/api-v2", name: "api-v2", agent_kind: "", session_slug: "", is_main: true }),
            worktree({ path: "/code/api-v2/.codex/worktrees/b", agent_kind: "codex", session_slug: "b", name: "b" }),
          ]),
        }),
      }),
    ],
    tasks: tasks({ tasks: [task({ repoPath: "", cwd: "/elsewhere/x" })] }),
  });
}

describe("scoping the Agents view to one repository", () => {
  it("keeps only that repository's rows, under any spelling or checkout of it", () => {
    const all = plane().rows;
    expect(all.length).toBeGreaterThanOrEqual(2);
    const paths = { caseInsensitive: true };
    for (const scope of ["/code/api", "/CODE/api/", "/code/api/.claude/worktrees/a"]) {
      const rows = scopeToRepository(all, scope, paths);
      expect(rows.map((row) => row.session), scope).toEqual(["a"]);
    }
  });

  it("keeps everything with no scope, and nothing unresolved under a scope", () => {
    const all = plane().rows;
    expect(scopeToRepository(all, null, { caseInsensitive: false })).toHaveLength(all.length);
    expect(scopeToRepository(all, "", { caseInsensitive: false })).toHaveLength(all.length);
    expect(scopeToRepository(all, "/code/api-v2", { caseInsensitive: false }).every((row) => row.repoPath === "/code/api-v2")).toBe(true);
    const unresolved = { ...all[0], id: "unresolved", repoPath: "", checkoutPath: null };
    expect(scopeToRepository([...all, unresolved], "/code/api", { caseInsensitive: false }).some((row) => row.id === "unresolved")).toBe(false);
    expect(scopeToRepository([...all, unresolved], null, { caseInsensitive: false }).some((row) => row.id === "unresolved")).toBe(true);
  });
});

