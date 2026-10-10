import { describe, expect, it } from "vitest";
import type { TaskCard } from "./client";
import { cardMatchesQuery, parseTaskQuery, type QueryContext } from "./taskQuery";
import { describeMatch, searchCards, type RankContext, type SearchOrder } from "./taskRank";

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

const NAMES: Record<string, string> = { r1: "GitPulse", r2: "Manvi" };
const ctx: RankContext = { repoName: (id) => NAMES[id] };
const filterCtx: QueryContext = { repository: (id) => (NAMES[id] ? { name: NAMES[id], identity_key: `local:/code/${NAMES[id]}/.git` } : undefined), nowSec: 1_000 };

/** The board's path: the shared filters first, then the words ranked over what passed. */
function search(cards: readonly TaskCard[], text: string, extra: Partial<RankContext> = {}, order: SearchOrder = "relevance") {
  const parsed = parseTaskQuery(text);
  const passed = cards.filter((c) => cardMatchesQuery(c, parsed, filterCtx));
  if (!parsed.words) return { cards: passed.map((c) => ({ card: c, score: 0, matches: [] })), relaxed: false };
  return searchCards(passed, parsed.words, { ...ctx, ...extra }, { order });
}
const titles = (cards: readonly TaskCard[], text: string, extra: Partial<RankContext> = {}, order: SearchOrder = "relevance") =>
  search(cards, text, extra, order).cards.map((r) => r.card.title);

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

  it("needs every clause and any word of an OR clause; exclusions are the shared parser's", () => {
    expect(titles(board, "login button")).toEqual(["Login button misaligned"]);
    expect(new Set(titles(board, "latency OR crash"))).toEqual(new Set(["App crashes on launch", "Reduce latency of graph view"]));
    expect(titles(board, "login -button")).toEqual(["Sign-in fails with passkey"]);
    expect(titles(board, '"sign in"')).toEqual(["Sign-in fails with passkey"]);
    expect(titles(board, 'login -"button mis"')).toEqual(["Sign-in fails with passkey"]);
  });

  it("offers partial matches, and says so, when nothing matches every word", () => {
    const outcome = search(board, "crash zzzqqq");
    expect(outcome.relaxed).toBe(true);
    expect(outcome.cards.map((r) => r.card.title)).toEqual(["App crashes on launch"]);
    expect(search(board, "zzzqqq")).toEqual({ cards: [], relaxed: false });
  });

  it("does not relax when the caller says the answer must be exact", () => {
    const parsed = parseTaskQuery("crash zzzqqq");
    expect(parsed.words && searchCards(board, parsed.words, ctx, { relax: false })).toEqual({ cards: [], relaxed: false });
  });

  it("credits the store's description hits only for the words the store matched", () => {
    const hidden = card({ title: "Unrelated title" });
    expect(titles([hidden], "segfault")).toEqual([]);
    const storeMatched = (id: string, word: string) => id === hidden.id && word.startsWith("segfault");
    expect(titles([hidden], "segfault", { storeMatched })).toEqual(["Unrelated title"]);
    expect(titles([hidden], "segfault", { storeMatched: () => false })).toEqual([]);
    expect(titles([hidden], "kernel", { storeMatched })).toEqual([]);
    const ranked = search([hidden], "segfault", { storeMatched }).cards[0];
    expect(describeMatch(ranked)).toContain("in description");
  });

  it("keeps board order under Board order and on ties", () => {
    const a = card({ title: "Fix login" }), b = card({ title: "Fix login" });
    expect(search([a, b], "login").cards.map((r) => r.card.id)).toEqual([a.id, b.id]);
    expect(titles(board, "login", {}, "board")).toEqual(["Sign-in fails with passkey", "Login button misaligned"]);
  });

  it("ranks a title match above a label match", () => {
    const titled = card({ title: "Auth token refresh" });
    const labelled = card({ title: "Token refresh", labels: ["auth"] });
    expect(titles([labelled, titled], "auth")).toEqual(["Auth token refresh", "Token refresh"]);
  });

  it("ranks only what the filters let through, in board order without words", () => {
    expect(titles(board, "kind:bug")).toEqual(["App crashes on launch"]);
    expect(titles(board, "label:auth,ui")).toEqual(["Sign-in fails with passkey", "Login button misaligned"]);
    expect(titles(board, "login label:ui")).toEqual(["Login button misaligned"]);
  });

  it("matches repository names as words too, once the catalog names them", () => {
    const c = card({ title: "Something", repository_ids: ["r9"] });
    const names: Record<string, string> = {};
    const late: RankContext = { repoName: (id) => names[id] };
    const words = parseTaskQuery("devmap").words;
    expect(words && searchCards([c], words, late).cards).toEqual([]);
    names.r9 = "devmap";
    expect(words && searchCards([c], words, late).cards.length).toBe(1);
  });
});

describe("performance", () => {
  it("ranks two thousand cards well inside a frame budget after the first pass", () => {
    const cards = Array.from({ length: 2000 }, (_, i) => card({ title: `Task ${i} about ${["login", "crash", "layout", "latency"][i % 4]} handling`, labels: [`l${i % 7}`] }));
    search(cards, "login handling");
    const start = performance.now();
    const outcome = search(cards, "logn handlng label:l1");
    expect(outcome.cards.length).toBeGreaterThan(0);
    expect(performance.now() - start).toBeLessThan(250);
  });
});
