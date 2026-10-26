/**
 * What the board fetches while a search or filter is active, and what it can
 * then say about coverage.
 *
 * Without one the board reads one page per column. With one it reads a
 * single status-less candidate set instead — the scope's tasks in board
 * order, 200 at a time — so the filters (`taskQuery.cardMatchesQuery` and
 * the Filters bar) and the ranking (`taskRank.ts`) reach every loaded task,
 * not just each column's first page. Beside it, one full-text request for the
 * search's most selective word (`serverWord`) finds tasks whose description
 * says it, which no card carries. Two requests, two cursors, one total each;
 * per-word or per-synonym fan-out would multiply requests and make paging
 * incoherent.
 *
 * Typing re-ranks without a request: the fetch key only changes with the
 * scope, the server word and a single-repository narrowing.
 */
import type { Page, Repository, Scope, TaskCard } from "./client";
import { STATUSES, type TaskStatus } from "./vocabulary";
import { isEmptyQuery, repositoryMatches, type ParsedTaskQuery } from "./taskQuery";
import { stem, STOPWORDS } from "./taskLexicon";
import { facetActive, type TaskFacet } from "./taskOrganize";
import { plural } from "../format";

/** One candidate page: the store's maximum. */
export const CANDIDATE_PAGE = 200;
/** Where loading more candidates stops; past it the reader narrows instead. */
export const MAX_CANDIDATES = 2000;

export interface SearchPlan {
  mode: "browse" | "search";
  /** The scope the candidate requests read; narrower than the board's for a single repository. */
  scope: Scope;
  /** The word sent to the store's full-text index, or null. */
  word: string | null;
  /** Changes exactly when the candidates must be fetched again. */
  key: string;
}

/**
 * The one word the store is asked for, so its full-text index can find tasks
 * whose *description* says it — the board's cards do not carry descriptions.
 *
 * The store ANDs every word it is given as a prefix, so sending them all only
 * narrows; the longest required word is the most selective. It goes as the
 * common prefix of the word and its stem, so "dependencies" still finds
 * "dependency". Null when there is no word worth a request — an `OR`
 * alternative is never one, since the other side may be the one that holds.
 */
export function serverWord(parsed: ParsedTaskQuery): string | null {
  const free = parsed.words;
  if (!free) return null;
  const required = free.clauses.filter((clause) => clause.length === 1).map((clause) => clause[0]);
  const candidates = required.length ? required : free.phrases.flatMap((phrase) => phrase.split(" "));
  const best = candidates
    .filter((word) => !STOPWORDS.has(word) && /^[\p{L}\p{N}]+$/u.test(word))
    // Longest first; on a tie, the word typed first.
    .reduce<string | undefined>((longest, word) => (!longest || word.length > longest.length ? word : longest), undefined);
  if (!best) return null;
  const root = stem(best);
  let prefix = 0;
  while (prefix < root.length && prefix < best.length && root[prefix] === best[prefix]) prefix++;
  const word = best.slice(0, Math.max(prefix, Math.min(best.length, 3)));
  return word.length >= 2 ? word : null;
}

/**
 * The repository a global search can be read from instead of every task: the
 * Filters bar's repository, or a single positive `repo:` that exactly one
 * repository of a fully loaded catalog answers to (`repositoryMatches`, the
 * filter's own test). Anything less certain stays global and is filtered
 * locally, because a narrower read would silently drop matching tasks.
 */
function narrowedRepository(parsed: ParsedTaskQuery, facet: TaskFacet, repositories: readonly Pick<Repository, "id" | "name" | "identity_key">[], catalogComplete: boolean): string | null {
  if (facet.repository !== "all") return facet.repository;
  const repos = parsed.terms.filter((term) => term.key === "repo");
  if (repos.length !== 1 || repos[0].negated || !catalogComplete) return null;
  const value = repos[0].value;
  const named = repositories.filter((repo) => repo.id.toLowerCase() === value || repositoryMatches(repo, value));
  return named.length === 1 ? named[0].id : null;
}

/**
 * The fetch a search needs. Inside a workspace or repository board the scope is kept: a repository read there could reach past the workspace.
 *
 * `hidingRepositories` is a filter like any other (`taskHiddenRepositories.ts`):
 * a column page that holds a hidden repository's tasks would count them in a
 * total the board cannot show, so the board reads candidates instead.
 */
export function planSearch(
  scope: Scope,
  parsed: ParsedTaskQuery,
  facet: TaskFacet,
  repositories: readonly Pick<Repository, "id" | "name" | "identity_key">[],
  catalogComplete: boolean,
  hidingRepositories = false,
): SearchPlan {
  if (isEmptyQuery(parsed) && !facetActive(facet) && !hidingRepositories) return { mode: "browse", scope, word: null, key: "browse" };
  const narrowed = scope.kind === "global" ? narrowedRepository(parsed, facet, repositories, catalogComplete) : null;
  const target: Scope = narrowed ? { kind: "repository", id: narrowed } : scope;
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
  /** Whether ranking reorders the board (a search with words, sorted by relevance). */
  ranked: boolean;
}

const fmt = (n: number) => n.toLocaleString("en-US");

/**
 * The coverage line under the search box. A partial answer always says it is
 * partial and how to widen it; it never reads as everything that matches.
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
  return parts.join(" · ");
}
