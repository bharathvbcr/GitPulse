import { describe, expect, it } from "vitest";
import type { Page, TaskCard } from "./client";
import { parseTaskQuery } from "./taskQuery";
import { emptyFacet } from "./taskOrganize";
import { bucketByStatus, canLoadMore, candidatesComplete, describeSearch, MAX_CANDIDATES, mergeCandidates, planSearch, serverWord } from "./taskSearchPlan";

const repos = [
  { id: "r1", name: "GitPulse", identity_key: "local:/Users/me/code/GitPulse/.git" },
  { id: "r2", name: "Manvi", identity_key: "github.com/acme/manvi" },
  { id: "r3", name: "manvi-docs", identity_key: "github.com/acme/manvi-docs" },
];
const plan = (text: string, scope: Parameters<typeof planSearch>[0] = { kind: "global" }, facet = emptyFacet(), complete = true) =>
  planSearch(scope, parseTaskQuery(text), facet, repos, complete);
function card(id: string, over: Partial<TaskCard> = {}): TaskCard {
  return {
    id, revision: 1, updated_at: 1, title: id, kind: "task", status: "inbox", priority: 2, severity: null, owner: null,
    due_at: null, labels: [], repository_ids: ["r1"], primary_repository_id: "r1", home_workspace_id: null, position: 1,
    archived: false, completed_at: null, ...over,
  };
}
const page = (items: TaskCard[], total: number, cursor: string | null = null): Page<TaskCard> => ({ items, total, shown: items.length, has_more: cursor !== null, next_cursor: cursor });

describe("serverWord", () => {
  it("sends the longest required word, trimmed to the prefix its forms share", () => {
    expect(serverWord(parseTaskQuery("fix login crash"))).toBe("login");
    expect(serverWord(parseTaskQuery("upgrade dependencies"))).toBe("dependenc");
    expect(serverWord(parseTaskQuery('"memory leak"'))).toBe("memory");
  });

  it("does not send a word only one side of an OR needs, nor a filter", () => {
    expect(serverWord(parseTaskQuery("login OR signin"))).toBeNull();
    expect(serverWord(parseTaskQuery("label:ui"))).toBeNull();
    expect(serverWord(parseTaskQuery("a"))).toBeNull();
    expect(serverWord(parseTaskQuery("-crash"))).toBeNull();
  });
});

describe("planSearch", () => {
  it("browses with nothing to search or filter by, and searches with a word, a filter or a Filters choice", () => {
    expect(plan("").mode).toBe("browse");
    expect(plan("priority:huge").mode).toBe("browse");
    expect(plan("login crash")).toMatchObject({ mode: "search", scope: { kind: "global" }, word: "login" });
    expect(plan("is:bug")).toMatchObject({ mode: "search", word: null });
    expect(plan("", { kind: "global" }, { ...emptyFacet(), priority: 1 }).mode).toBe("search");
  });

  it("only refetches when the scope, the server word or a narrowing changes", () => {
    const a = plan("login crash");
    expect(plan("login crash label:ui -flaky").key).toBe(a.key);
    expect(plan("login bug").key).toBe(a.key);
    expect(plan("authentication crash").key).not.toBe(a.key);
  });

  it("narrows the global board to one unambiguous repository", () => {
    expect(plan("repo:gitpulse").scope).toEqual({ kind: "repository", id: "r1" });
    expect(plan("repo:r1").scope).toEqual({ kind: "repository", id: "r1" });
    // "manvi" names two repositories; ambiguous stays global and filters locally.
    expect(plan("repo:manvi").scope).toEqual({ kind: "global" });
    expect(plan("repo:gitpulse,manvi-docs").scope).toEqual({ kind: "global" });
    expect(plan("repo:gitpulse repo:manvi-docs").scope).toEqual({ kind: "global" });
    expect(plan("-repo:gitpulse").scope).toEqual({ kind: "global" });
    expect(plan("repo:gitpulse", { kind: "workspace", id: "w" }).scope).toEqual({ kind: "workspace", id: "w" });
    // A catalog still paging could hold a second repository that answers.
    expect(plan("repo:gitpulse", { kind: "global" }, emptyFacet(), false).scope).toEqual({ kind: "global" });
  });

  it("narrows to the Filters bar's repository, which every shown card must link", () => {
    expect(plan("", { kind: "global" }, { ...emptyFacet(), repository: "r2" }).scope).toEqual({ kind: "repository", id: "r2" });
    expect(plan("", { kind: "repository", id: "r1" }, { ...emptyFacet(), repository: "r2" }).scope).toEqual({ kind: "repository", id: "r1" });
  });
});

describe("candidates", () => {
  it("merges pages in board order without duplicates and records description hits", () => {
    const first = mergeCandidates(null, page([card("b", { position: 2 }), card("a", { position: 1 })], 5, "c1"), page([card("z", { position: 9 })], 1));
    expect(first.cards.map((c) => c.id)).toEqual(["a", "b", "z"]);
    expect([...first.textHits]).toEqual(["z"]);
    expect(candidatesComplete(first)).toBe(false);
    expect(canLoadMore(first)).toBe(true);
    const second = mergeCandidates(first, page([card("z", { position: 9 }), card("c", { position: 3 })], 5), null);
    expect(second.cards.map((c) => c.id)).toEqual(["a", "b", "c", "z"]);
    expect(second.broad).toEqual({ total: 5, loaded: 4, cursor: null });
    expect(second.text).toEqual({ total: 1, loaded: 1, cursor: null });
    expect(candidatesComplete(second)).toBe(true);
    expect(canLoadMore(second)).toBe(false);
  });

  it("stops offering more at the candidate ceiling", () => {
    const many = Array.from({ length: MAX_CANDIDATES }, (_, i) => card(`t${i}`, { position: i }));
    expect(canLoadMore(mergeCandidates(null, page(many, MAX_CANDIDATES + 10, "more"), null))).toBe(false);
  });

  it("buckets candidates into every column", () => {
    const columns = bucketByStatus([card("a", { status: "done" }), card("b", { status: "inbox" })]);
    expect(Object.keys(columns)).toEqual(["inbox", "backlog", "ready", "in_progress", "review", "done"]);
    expect(columns.done).toEqual({ items: [expect.objectContaining({ id: "a" })], total: 1, shown: 1, has_more: false, next_cursor: null });
  });
});

describe("describeSearch", () => {
  const complete = mergeCandidates(null, page([card("a")], 1), page([], 0));
  const partial = mergeCandidates(null, page([card("a")], 900, "next"), null);

  it("states a complete answer plainly", () => {
    expect(describeSearch({ matched: 1, set: complete, word: "login", relaxed: false, ranked: true })).toBe(
      "1 match in all 1 task · descriptions searched for “login…” (0) · ranked by title, labels, type, owner and repository, with word forms, related words and close spellings",
    );
  });

  it("never lets a partial answer read as complete", () => {
    const text = describeSearch({ matched: 0, set: partial, word: null, relaxed: false, ranked: false });
    expect(text).toContain("1 of 900 tasks");
    expect(text).toContain("more may match");
  });

  it("says when results only match some of the words", () => {
    expect(describeSearch({ matched: 2, set: complete, word: null, relaxed: true, ranked: true })).toContain("no task has every word");
  });
});
