import { describe, expect, it } from "vitest";
import type { TaskCard } from "./client";
import { parseTaskQuery } from "./taskQuery";
import { cardInRepo, countBy, describeMatch, facetCounts, matchesFilters, searchCards, type RankContext } from "./taskRank";

let next = 0;
function card(over: Partial<TaskCard> = {}): TaskCard {
  next++;
  return {
    id: `t${next}`, revision: 1, updated_at: 1, title: "Untitled", kind: "task", status: "inbox",
    priority: 2, severity: null, owner: null, due_at: null, labels: [], repository_ids: ["r1"],
    primary_repository_id: "r1", home_workspace_id: null, position: next, archived: false, completed_at: null,
    ...over,
  };
}

const ctx: RankContext = { nowSec: 1_000, repoName: (id) => ({ r1: "GitPulse", r2: "Manvi" })[id] };
const titles = (cards: readonly TaskCard[], text: string, extra: Partial<RankContext> = {}, order: "relevance" | "board" = "relevance") =>
  searchCards(cards, parseTaskQuery(text), { ...ctx, ...extra }, order).cards.map((r) => r.card.title);

describe("searchCards", () => {
  const board = [
    card({ title: "Polish settings layout" }),
    card({ title: "Sign-in fails with passkey", labels: ["auth"] }),
    card({ title: "App crashes on launch", kind: "bug" }),
    card({ title: "Reduce latency of graph view" }),
    card({ title: "Login button misaligned", labels: ["ui"] }),
  ];

  it("finds related words, word forms and close spellings, best first", () => {
    expect(titles(board, "login")).toEqual(["Login button misaligned", "Sign-in fails with passkey"]);
    expect(titles(board, "crash")).toEqual(["App crashes on launch"]);
    expect(titles(board, "crahs")).toEqual(["App crashes on launch"]);
    expect(titles(board, "slow")).toEqual(["Reduce latency of graph view"]);
    expect(titles(board, "sett")).toEqual(["Polish settings layout"]);
  });

  it("needs every clause, any word of an OR clause, and honours exclusions", () => {
    expect(titles(board, "login button")).toEqual(["Login button misaligned"]);
    expect(new Set(titles(board, "latency OR crash"))).toEqual(new Set(["App crashes on launch", "Reduce latency of graph view"]));
    expect(titles(board, "login -button")).toEqual(["Sign-in fails with passkey"]);
    expect(titles(board, '"sign in"')).toEqual(["Sign-in fails with passkey"]);
    expect(titles(board, 'login -"button mis"')).toEqual(["Sign-in fails with passkey"]);
  });

  it("offers partial matches, and says so, when nothing matches every word", () => {
    const outcome = searchCards(board, parseTaskQuery("crash zzzqqq"), ctx);
    expect(outcome.relaxed).toBe(true);
    expect(outcome.cards.map((r) => r.card.title)).toEqual(["App crashes on launch"]);
    expect(searchCards(board, parseTaskQuery("zzzqqq"), ctx)).toEqual({ cards: [], relaxed: false });
  });

  it("credits the store's description hits for the server word only", () => {
    const hidden = card({ title: "Unrelated title" });
    expect(titles([hidden], "segfault")).toEqual([]);
    expect(titles([hidden], "segfault", { serverWord: "segfault", serverHits: new Set([hidden.id]) })).toEqual(["Unrelated title"]);
    expect(titles([hidden], "segfault", { serverWord: "segfault", serverHits: new Set() })).toEqual([]);
    const ranked = searchCards([hidden], parseTaskQuery("segfault"), { ...ctx, serverWord: "segfault", serverHits: new Set([hidden.id]) }).cards[0];
    expect(describeMatch(ranked)).toContain("in description");
  });

  it("keeps board order under Board order and on ties", () => {
    const a = card({ title: "Fix login" }), b = card({ title: "Fix login" });
    expect(searchCards([a, b], parseTaskQuery("login"), ctx).cards.map((r) => r.card.id)).toEqual([a.id, b.id]);
    expect(titles(board, "login", {}, "board")).toEqual(["Sign-in fails with passkey", "Login button misaligned"]);
  });

  it("ranks a title match above a label match and a rare word above a common one", () => {
    const titled = card({ title: "Auth token refresh" });
    const labelled = card({ title: "Token refresh", labels: ["auth"] });
    expect(titles([labelled, titled], "auth")).toEqual(["Auth token refresh", "Token refresh"]);
  });

  it("filters without words keep board order", () => {
    expect(titles(board, "kind:bug")).toEqual(["App crashes on launch"]);
    expect(titles(board, "label:auth,ui")).toEqual(["Sign-in fails with passkey", "Login button misaligned"]);
  });
});

describe("filters", () => {
  const f = (c: TaskCard, text: string) => matchesFilters(c, parseTaskQuery(text), ctx);

  it("matches each qualifier exactly and case-insensitively", () => {
    const c = card({ owner: "@Alice", labels: ["UI", "a11y"], kind: "Bug", severity: "high", priority: 0, status: "review", repository_ids: ["r1", "r2"] });
    expect(f(c, "owner:alice")).toBe(true);
    expect(f(c, "owner:@bob")).toBe(false);
    expect(f(c, "label:ui label:A11Y")).toBe(true);
    expect(f(c, "label:ui label:missing")).toBe(false);
    expect(f(c, "-label:ui")).toBe(false);
    expect(f(c, "type:bug severity:HIGH priority:urgent status:review")).toBe(true);
    expect(f(c, "priority:low")).toBe(false);
    expect(f(c, "repo:manvi")).toBe(true);
    expect(f(c, "repo:git")).toBe(true);
    expect(f(c, "-repo:manvi")).toBe(false);
    expect(f(c, "is:multi-repo is:assigned is:labeled")).toBe(true);
    expect(f(c, "is:unassigned")).toBe(false);
  });

  it("reads due dates against now", () => {
    expect(f(card({ due_at: 500 }), "is:overdue")).toBe(true);
    expect(f(card({ due_at: 2_000 }), "is:soon")).toBe(true);
    expect(f(card({ due_at: null }), "is:no-due")).toBe(true);
    expect(f(card({ due_at: null }), "is:due")).toBe(false);
    expect(f(card({ due_at: 500 }), "is:soon,overdue")).toBe(true);
  });

  it("resolves repositories by id, name, then name prefix", () => {
    const c = card({ repository_ids: ["r2"] });
    expect(cardInRepo(c, "r2", ctx.repoName)).toBe(true);
    expect(cardInRepo(c, "MANVI", ctx.repoName)).toBe(true);
    expect(cardInRepo(c, "man", ctx.repoName)).toBe(true);
    expect(cardInRepo(c, "m", ctx.repoName)).toBe(false);
    expect(cardInRepo(c, "gitpulse", ctx.repoName)).toBe(false);
  });

  it("matches repository names as words too, once the catalog names them", () => {
    const c = card({ title: "Something", repository_ids: ["r9"] });
    const names: Record<string, string> = {};
    const late: RankContext = { nowSec: 0, repoName: (id) => names[id] };
    expect(searchCards([c], parseTaskQuery("devmap"), late).cards).toEqual([]);
    names.r9 = "devmap";
    expect(searchCards([c], parseTaskQuery("devmap"), late).cards.length).toBe(1);
  });
});

describe("countBy", () => {
  it("counts each value once per card, most common first", () => {
    const counts = countBy([card({ labels: ["a", "b", "a"] }), card({ labels: ["b"] }), card({ labels: [" "] })], (c) => c.labels);
    expect(counts).toEqual([{ value: "b", count: 2 }, { value: "a", count: 1 }]);
  });
});

describe("facetCounts", () => {
  it("counts each value under the rest of the query, leaving its own key out", () => {
    const cards = [card({ title: "login", labels: ["ui"] }), card({ title: "login", labels: ["auth"] }), card({ title: "other", labels: ["ui"] })];
    const counts = facetCounts(cards, parseTaskQuery("login label:ui"), ctx, "label", (c) => c.labels);
    expect(counts).toEqual([{ value: "auth", count: 1 }, { value: "ui", count: 1 }]);
  });
});

describe("performance", () => {
  it("ranks two thousand cards well inside a frame budget after the first pass", () => {
    const cards = Array.from({ length: 2000 }, (_, i) => card({ title: `Task ${i} about ${["login", "crash", "layout", "latency"][i % 4]} handling`, labels: [`l${i % 7}`] }));
    searchCards(cards, parseTaskQuery("login handling"), ctx);
    const start = performance.now();
    const outcome = searchCards(cards, parseTaskQuery("logn handlng label:l1"), ctx);
    expect(outcome.cards.length).toBeGreaterThan(0);
    expect(performance.now() - start).toBeLessThan(250);
  });
});
