import { describe, expect, it } from "vitest";
import {
  MAX_HIDDEN_REPOSITORIES,
  hiddenByRepository,
  hiddenRepositoryNote,
  hidesRepositories,
  sanitizeHiddenRepositories,
  toggleHiddenRepository,
} from "./taskHiddenRepositories";
import { emptyFacet } from "./taskOrganize";
import { parseTaskQuery } from "./taskQuery";
import { planSearch } from "./taskSearchPlan";

const card = (primary: string, ...links: string[]) => ({ primary_repository_id: primary, repository_ids: links.length ? links : [primary] });

describe("sanitizeHiddenRepositories", () => {
  it("keeps distinct trimmed ids in their stored order", () => {
    expect(sanitizeHiddenRepositories([" repo-1 ", "repo-0", "repo-1"])).toEqual(["repo-1", "repo-0"]);
  });

  it("drops what is not an id rather than refusing the whole list", () => {
    expect(sanitizeHiddenRepositories(["repo-0", "", "   ", 7, null, {}, "x".repeat(257)])).toEqual(["repo-0"]);
  });

  it("reads anything that is not an array as nothing hidden", () => {
    for (const value of [undefined, null, "repo-0", 3, { 0: "repo-0" }]) expect(sanitizeHiddenRepositories(value)).toEqual([]);
  });

  it("is bounded, keeping the first ids", () => {
    const many = Array.from({ length: MAX_HIDDEN_REPOSITORIES + 10 }, (_, i) => `repo-${i}`);
    const kept = sanitizeHiddenRepositories(many);
    expect(kept).toHaveLength(MAX_HIDDEN_REPOSITORIES);
    expect(kept[0]).toBe("repo-0");
  });
});

describe("toggleHiddenRepository", () => {
  it("adds an id once and takes it out again", () => {
    const hidden = toggleHiddenRepository([], "repo-0");
    expect(hidden).toEqual(["repo-0"]);
    expect(toggleHiddenRepository(hidden, "repo-0")).toEqual([]);
  });

  it("ignores an empty id", () => {
    expect(toggleHiddenRepository(["repo-0"], "  ")).toEqual(["repo-0"]);
  });

  it("does not grow past the bound", () => {
    const full = Array.from({ length: MAX_HIDDEN_REPOSITORIES }, (_, i) => `repo-${i}`);
    expect(toggleHiddenRepository(full, "one-more")).toEqual(full);
  });
});

describe("hidesRepositories", () => {
  it("applies on the global and workspace boards, never on a repository's own", () => {
    expect(hidesRepositories({ kind: "global" }, ["repo-0"])).toBe(true);
    expect(hidesRepositories({ kind: "workspace", id: "w" }, ["repo-0"])).toBe(true);
    expect(hidesRepositories({ kind: "repository", id: "repo-0" }, ["repo-0"])).toBe(false);
    expect(hidesRepositories({ kind: "global" }, [])).toBe(false);
  });
});

describe("hiddenByRepository", () => {
  const hidden = new Set(["repo-0"]);

  it("goes by the primary repository, not by every link", () => {
    expect(hiddenByRepository(card("repo-0", "repo-0", "repo-1"), hidden, null)).toBe(true);
    expect(hiddenByRepository(card("repo-1", "repo-1", "repo-0"), hidden, null)).toBe(false);
  });

  it("falls back to the first link when a card names no primary", () => {
    expect(hiddenByRepository(card("", "repo-0"), hidden, null)).toBe(true);
    expect(hiddenByRepository({ primary_repository_id: "", repository_ids: [] }, hidden, null)).toBe(false);
  });

  it("shows a hidden repository's tasks when the reader narrowed to that repository", () => {
    expect(hiddenByRepository(card("repo-0"), hidden, "repo-0")).toBe(false);
    expect(hiddenByRepository(card("repo-0"), hidden, "repo-1")).toBe(true);
  });
});

describe("hiddenRepositoryNote", () => {
  it("names the tasks and repositories kept off the board", () => {
    expect(hiddenRepositoryNote({ tasks: 3, repositories: 1, complete: true })).toBe("3 tasks from 1 hidden repository are not on this board.");
  });

  it("says when the count covers only the tasks loaded so far", () => {
    expect(hiddenRepositoryNote({ tasks: 1, repositories: 2, complete: false })).toBe("1 task from 2 hidden repositories is not on this board, counting only the tasks loaded so far.");
  });

  it("still says repositories are hidden when no loaded task belongs to them", () => {
    expect(hiddenRepositoryNote({ tasks: 0, repositories: 2, complete: true })).toBe("2 repositories are hidden; none of this board's tasks belong to them.");
  });
});

describe("planSearch with hidden repositories", () => {
  const repos = [{ id: "repo-0", name: "GitPulse", identity_key: "local:/g/.git" }];

  it("reads one candidate set so a hidden repository's tasks never count as the column's", () => {
    const plan = planSearch({ kind: "global" }, parseTaskQuery(""), emptyFacet(), repos, true, true);
    expect(plan.mode).toBe("search");
    expect(plan.scope).toEqual({ kind: "global" });
    expect(plan.word).toBeNull();
  });

  it("browses columns as before when nothing is hidden", () => {
    expect(planSearch({ kind: "global" }, parseTaskQuery(""), emptyFacet(), repos, true, false).mode).toBe("browse");
  });
});
