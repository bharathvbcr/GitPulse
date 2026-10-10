import { describe, expect, it } from "vitest";
import type { Page, TaskCard } from "./client";
import { parseTaskQuery } from "./taskQuery";
import { bucketByStatus, canLoadMore, candidatesComplete, describeSearch, MAX_CANDIDATES, mergeCandidates, planSearch } from "./taskSearchPlan";

const repos = [{ id: "r1", name: "GitPulse" }, { id: "r2", name: "Manvi" }, { id: "r3", name: "manvi" }];
function card(id: string, over: Partial<TaskCard> = {}): TaskCard {
  return {
    id, revision: 1, updated_at: 1, title: id, kind: "task", status: "inbox", priority: 2, severity: null, owner: null,
    due_at: null, labels: [], repository_ids: ["r1"], primary_repository_id: "r1", home_workspace_id: null, position: 1,
    archived: false, completed_at: null, ...over,
  };
}
const page = (items: TaskCard[], total: number, cursor: string | null = null): Page<TaskCard> => ({ items, total, shown: items.length, has_more: cursor !== null, next_cursor: cursor });

describe("planSearch", () => {
  it("browses without a query and searches with one", () => {
    expect(planSearch({ kind: "global" }, parseTaskQuery(""), repos).mode).toBe("browse");
    const plan = planSearch({ kind: "global" }, parseTaskQuery("login crash"), repos);
    expect(plan).toMatchObject({ mode: "search", scope: { kind: "global" }, word: "login" });
  });

  it("only refetches when the scope, the server word or a narrowing changes", () => {
    const a = planSearch({ kind: "global" }, parseTaskQuery("login crash"), repos);
    const b = planSearch({ kind: "global" }, parseTaskQuery("login crash label:ui -flaky"), repos);
    expect(a.key).toBe(b.key);
    expect(planSearch({ kind: "global" }, parseTaskQuery("login bug"), repos).key).toBe(a.key);
    expect(planSearch({ kind: "global" }, parseTaskQuery("authentication crash"), repos).key).not.toBe(a.key);
  });

  it("narrows the global board to one unambiguous repository", () => {
    expect(planSearch({ kind: "global" }, parseTaskQuery("repo:gitpulse"), repos).scope).toEqual({ kind: "repository", id: "r1" });
    expect(planSearch({ kind: "global" }, parseTaskQuery("repo:r1"), repos).scope).toEqual({ kind: "repository", id: "r1" });
    // Two repositories share the name; ambiguous stays global and filters locally.
    expect(planSearch({ kind: "global" }, parseTaskQuery("repo:manvi"), repos).scope).toEqual({ kind: "global" });
    expect(planSearch({ kind: "global" }, parseTaskQuery("repo:gitpulse,manvi"), repos).scope).toEqual({ kind: "global" });
    expect(planSearch({ kind: "global" }, parseTaskQuery("-repo:gitpulse"), repos).scope).toEqual({ kind: "global" });
    expect(planSearch({ kind: "workspace", id: "w" }, parseTaskQuery("repo:gitpulse"), repos).scope).toEqual({ kind: "workspace", id: "w" });
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
    expect(describeSearch({ matched: 1, set: complete, word: "login", relaxed: false, warnings: [], ranked: true })).toBe(
      "1 match in all 1 task · descriptions searched for “login…” (0) · ranked by title, labels, type, owner and repository, with word forms, related words and close spellings",
    );
  });

  it("never lets a partial answer read as complete", () => {
    const text = describeSearch({ matched: 0, set: partial, word: null, relaxed: false, warnings: ["Unknown status"], ranked: false });
    expect(text).toContain("1 of 900 tasks");
    expect(text).toContain("more may match");
    expect(text).toContain("Unknown status");
  });

  it("says when results only match some of the words", () => {
    expect(describeSearch({ matched: 2, set: complete, word: null, relaxed: true, warnings: [], ranked: true })).toContain("no task has every word");
  });
});
