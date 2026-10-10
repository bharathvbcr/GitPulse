/**
 * What the board fetches while a query is active, and what it can then say
 * about coverage.
 *
 * Without a query the board reads one page per column. With one it reads a
 * single status-less candidate set instead — the scope's tasks in board
 * order, 200 at a time — and ranks them locally (`taskRank.ts`), so synonyms,
 * typos and filters reach every loaded task, not just the words the store's
 * index can prefix-match. Beside it, one full-text request for the query's
 * most selective word (`taskQuery.serverWord`) finds tasks whose description
 * says it, which no card carries. Two requests, two cursors, one total each;
 * per-word or per-synonym fan-out would multiply requests and make paging
 * incoherent.
 *
 * Typing re-ranks without a request: the fetch key only changes with the
 * scope, the server word and a single-repository narrowing.
 */
import type { Page, Scope, TaskCard } from "./client";
import { STATUSES, type TaskStatus } from "./vocabulary";
import { isEmptyQuery, serverWord, type ParsedQuery } from "./taskQuery";
import { plural } from "../format";

/** One candidate page: the store's maximum. */
export const CANDIDATE_PAGE = 200;
/** Where loading more candidates stops; past it the reader narrows instead. */
export const MAX_CANDIDATES = 2000;

export interface SearchPlan {
  mode: "browse" | "search";
  /** The scope the candidate requests read; narrower than the board's for a single `repo:`. */
  scope: Scope;
  /** The word sent to the store's full-text index, or null. */
  word: string | null;
  /** Changes exactly when the candidates must be fetched again. */
  key: string;
}

/**
 * The fetch a query needs. A single positive `repo:` naming exactly one
 * registered repository narrows the global scope to it on the server, so its
 * tasks are counted exactly; inside a workspace or repository board the
 * filter stays client-side, because a repository scope there could read tasks
 * from outside the workspace.
 */
export function planSearch(scope: Scope, parsed: ParsedQuery, repositories: readonly { id: string; name: string }[]): SearchPlan {
  if (isEmptyQuery(parsed)) return { mode: "browse", scope, word: null, key: "browse" };
  let target: Scope = scope;
  const repos = parsed.qualifiers.filter((q) => q.key === "repo");
  if (scope.kind === "global" && repos.length === 1 && !repos[0].negated && repos[0].values.length === 1) {
    const value = repos[0].values[0];
    const folded = value.toLowerCase();
    const named = repositories.filter((repo) => repo.id === value || repo.name.toLowerCase() === folded);
    if (named.length === 1) target = { kind: "repository", id: named[0].id };
  }
  const word = serverWord(parsed);
  return { mode: "search", scope: target, word, key: JSON.stringify(["search", target, word]) };
}

/** One request's progress through its pages. */
export interface CandidateSource { total: number; loaded: number; cursor: string | null }

export interface CandidateSet {
  /** Every candidate, in board order, deduplicated by id. */
  cards: TaskCard[];
  /** The scope's tasks, paged. */
  broad: CandidateSource;
  /** The full-text request for the server word, when there is one. */
  text: CandidateSource | null;
  /** Ids the full-text request returned. */
  textHits: ReadonlySet<string>;
}

function source(previous: CandidateSource | null, page: Page<TaskCard>): CandidateSource {
  return { total: page.total, loaded: (previous?.loaded ?? 0) + page.items.length, cursor: page.has_more ? page.next_cursor : null };
}

function boardOrder(a: TaskCard, b: TaskCard): number {
  return a.position - b.position || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/** `previous` with the next pages folded in; a null page leaves that source as it was. */
export function mergeCandidates(previous: CandidateSet | null, broad: Page<TaskCard> | null, text: Page<TaskCard> | null): CandidateSet {
  const byId = new Map((previous?.cards ?? []).map((card) => [card.id, card]));
  for (const card of broad?.items ?? []) byId.set(card.id, card);
  for (const card of text?.items ?? []) byId.set(card.id, card);
  const hits = new Set(previous?.textHits ?? []);
  for (const card of text?.items ?? []) hits.add(card.id);
  return {
    cards: [...byId.values()].sort(boardOrder),
    broad: broad ? source(previous?.broad ?? null, broad) : previous?.broad ?? { total: 0, loaded: 0, cursor: null },
    text: text ? source(previous?.text ?? null, text) : previous?.text ?? null,
    textHits: hits,
  };
}

/** Whether every task in the searched scope is loaded. */
export function candidatesComplete(set: CandidateSet): boolean {
  return set.broad.cursor === null;
}

/** Whether another page can be read without passing `MAX_CANDIDATES`. */
export function canLoadMore(set: CandidateSet): boolean {
  return (set.broad.cursor !== null || set.text?.cursor != null) && set.cards.length < MAX_CANDIDATES;
}

/**
 * The candidates as the board's columns: each status's cards in board order.
 * Totals are the loaded counts; nothing pages per column in search mode.
 */
export function bucketByStatus(cards: readonly TaskCard[]): Partial<Record<TaskStatus, Page<TaskCard>>> {
  const columns: Partial<Record<TaskStatus, Page<TaskCard>>> = {};
  for (const status of STATUSES) {
    const items = cards.filter((card) => card.status === status);
    columns[status] = { items, total: items.length, shown: items.length, has_more: false, next_cursor: null };
  }
  return columns;
}

export interface SearchReport {
  matched: number;
  set: CandidateSet;
  word: string | null;
  relaxed: boolean;
  warnings: readonly string[];
  /** Whether ranking reorders the board (a query with words, sorted by relevance). */
  ranked: boolean;
}

const fmt = (n: number) => n.toLocaleString("en-US");

/**
 * The line under the search box. A partial answer always says it is partial
 * and how to widen it; it never reads as everything that matches.
 */
export function describeSearch(report: SearchReport): string {
  const { set } = report;
  const parts: string[] = [];
  const searched = candidatesComplete(set)
    ? `all ${plural(set.broad.total, "task")}`
    : `${fmt(set.broad.loaded)} of ${fmt(set.broad.total)} tasks`;
  parts.push(`${plural(report.matched, "match", "matches")} in ${searched}`);
  if (report.relaxed) parts.push("no task has every word, so these have some of them");
  if (report.word) {
    const textCount = set.text ? (set.text.cursor ? `${fmt(set.text.loaded)} of ${fmt(set.text.total)}` : fmt(set.text.total)) : "0";
    parts.push(`descriptions searched for “${report.word}…” (${textCount})`);
  }
  if (report.ranked) parts.push("ranked by title, labels, type, owner and repository, with word forms, related words and close spellings");
  if (!candidatesComplete(set)) {
    parts.push(set.cards.length >= MAX_CANDIDATES
      ? `stopped at ${fmt(MAX_CANDIDATES)} loaded tasks — narrow with repo: or label: to reach the rest`
      : "more may match: load more tasks to search them");
  }
  return [...parts, ...report.warnings].join(" · ");
}
