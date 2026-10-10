/**
 * Which loaded tasks a parsed query matches, and in what order.
 *
 * Filters (`repo:`, `label:`, `is:` …) are exact and decide membership. Words
 * are ranked: each is compared with a card's title, labels, type, owner,
 * severity and repository names at falling strengths — exact, prefix, stem,
 * synonym, substring, typo — and weighted by how rare it is among the
 * candidates, so "auth" outranks "fix". Every clause must match; when no
 * task matches them all, the tasks matching some are offered instead and the
 * result says so.
 *
 * A card does not carry its description. The store's full-text index does,
 * so the tasks it returned for the query's one server word (`serverHits`) are
 * credited with that word even when the card's own fields do not show it.
 */
import type { TaskCard } from "./client";
import { dueState } from "./taskOrganize";
import { editDistance, fold, stem, synonymsOf, typoBudget, words } from "./taskLexicon";
import type { ParsedQuery, Qualifier, QualifierKey } from "./taskQuery";

export type SearchOrder = "relevance" | "board";

export type MatchField = "title" | "label" | "kind" | "owner" | "severity" | "repo" | "description";

/** How strongly a field's words weigh against each other. */
const FIELD_WEIGHT: Readonly<Record<MatchField, number>> = {
  title: 3, label: 2, kind: 1.5, owner: 1.5, severity: 1, repo: 1, description: 1.2,
};

/** How close a word came, from the same word down to a typo. */
export const LEVEL = { exact: 1, prefix: 0.85, stem: 0.8, synonym: 0.6, substring: 0.5, typo: 0.45 } as const;
export type MatchLevel = keyof typeof LEVEL;

export interface RankContext {
  nowSec: number;
  repoName: (id: string) => string | undefined;
  /** Tasks the store's full-text index returned for `serverWord`. */
  serverHits?: ReadonlySet<string>;
  /** The word those hits were asked for (`taskQuery.serverWord`). */
  serverWord?: string | null;
}

/** Why a card matched one word: shown as the card's match hint. */
export interface WordMatch { word: string; field: MatchField; level: MatchLevel; via: string }

export interface RankedCard {
  card: TaskCard;
  score: number;
  /** The best match per clause that matched, strongest first. */
  matches: WordMatch[];
}

export interface SearchOutcome {
  cards: RankedCard[];
  /** No task matched every word; these match some of them. */
  relaxed: boolean;
}

interface FieldWords { field: MatchField; words: string[]; stems: string[] }
interface CardDoc { fields: FieldWords[]; title: string; labels: string[]; repos: string }

/**
 * Normalized card text, built once per card object: a changed task arrives as
 * a new card. Rebuilt when the repository names it read have changed, since
 * the catalog can load after the board.
 */
const docs = new WeakMap<TaskCard, CardDoc>();

function fieldWords(field: MatchField, text: string): FieldWords {
  const list = words(text);
  return { field, words: list, stems: list.map(stem) };
}

function cardDoc(card: TaskCard, repoName: RankContext["repoName"]): CardDoc {
  const repos = (card.repository_ids ?? []).map((id) => repoName(id) ?? "").join(" ");
  const cached = docs.get(card);
  if (cached && cached.repos === repos) return cached;
  const labels = (card.labels ?? []).map((label) => fold(label).trim());
  const doc: CardDoc = {
    fields: [
      fieldWords("title", card.title ?? ""),
      fieldWords("label", (card.labels ?? []).join(" ")),
      fieldWords("kind", card.kind ?? ""),
      fieldWords("owner", card.owner ?? ""),
      fieldWords("severity", card.severity ?? ""),
      fieldWords("repo", repos),
    ],
    title: words(card.title ?? "").join(" "),
    labels: labels.map((label) => words(label).join(" ")),
    repos,
  };
  docs.set(card, doc);
  return doc;
}

/** The closest one word comes to any word of one field. */
function compareWord(query: string, queryStem: string, field: FieldWords): { level: MatchLevel; via: string } | null {
  let best: { level: MatchLevel; via: string } | null = null;
  const better = (level: MatchLevel, via: string) => {
    if (!best || LEVEL[level] > LEVEL[best.level]) best = { level, via };
  };
  const synonyms = synonymsOf(queryStem);
  const budget = typoBudget(query.length);
  for (let i = 0; i < field.words.length; i++) {
    const word = field.words[i], wordStem = field.stems[i];
    if (word === query) return { level: "exact", via: word };
    if (query.length >= 2 && word.startsWith(query)) better("prefix", word);
    else if (wordStem === queryStem) better("stem", word);
    else if (synonyms.has(wordStem)) better("synonym", word);
    else if (query.length >= 3 && word.includes(query)) better("substring", word);
    else if (budget > 0 && word.length >= 4 && (editDistance(word, query, budget) <= budget || editDistance(wordStem, queryStem, budget) <= budget)) better("typo", word);
  }
  return best;
}

/** The best match of one word anywhere on the card, weighted by field. */
function matchWord(word: string, doc: CardDoc, card: TaskCard, ctx: RankContext): { match: WordMatch; strength: number } | null {
  const wordStem = stem(word);
  let best: { match: WordMatch; strength: number } | null = null;
  for (const field of doc.fields) {
    const hit = compareWord(word, wordStem, field);
    if (!hit) continue;
    const strength = LEVEL[hit.level] * FIELD_WEIGHT[field.field];
    if (!best || strength > best.strength) best = { match: { word, field: field.field, level: hit.level, via: hit.via }, strength };
  }
  if (!best && ctx.serverWord && ctx.serverHits?.has(card.id) && word.startsWith(ctx.serverWord)) {
    best = { match: { word, field: "description", level: "prefix", via: word }, strength: LEVEL.prefix * FIELD_WEIGHT.description };
  }
  return best;
}

/** A word the card must not carry: only its own forms count, not synonyms or typos. */
function carries(word: string, doc: CardDoc): boolean {
  const wordStem = stem(word);
  return doc.fields.some((field) => field.words.some((w, i) => w === word || w.startsWith(word) || field.stems[i] === wordStem));
}

function containsPhrase(doc: CardDoc, phrase: string): boolean {
  return doc.title.includes(phrase) || doc.labels.some((label) => label.includes(phrase));
}

function sameName(a: string, b: string): boolean {
  return fold(a).replace(/^@/, "").trim() === fold(b).replace(/^@/, "").trim();
}

/** Whether a card's repositories include one named by `value` (name, then id, then name prefix). */
export function cardInRepo(card: TaskCard, value: string, repoName: RankContext["repoName"]): boolean {
  const ids = card.repository_ids ?? [];
  if (ids.some((id) => id === value || sameName(repoName(id) ?? "", value))) return true;
  const wanted = fold(value).trim();
  return wanted.length >= 2 && ids.some((id) => fold(repoName(id) ?? "").startsWith(wanted));
}

function qualifierHolds(card: TaskCard, q: Qualifier, ctx: RankContext): boolean {
  return q.values.some((value) => {
    switch (q.key) {
      case "repo": return cardInRepo(card, value, ctx.repoName);
      case "label": return (card.labels ?? []).some((label) => sameName(label, value));
      case "owner": return sameName(card.owner ?? "", value) && (card.owner ?? "").trim() !== "";
      case "kind": return sameName(card.kind ?? "", value);
      case "severity": return sameName(card.severity ?? "", value);
      case "status": return card.status === value;
      case "priority": return card.priority === Number(value);
      case "is": {
        const due = dueState(card.due_at, ctx.nowSec);
        switch (value) {
          case "overdue": return due === "overdue";
          case "soon": return due === "soon";
          case "due": return due !== "none";
          case "no-due": return due === "none";
          case "assigned": return !!card.owner?.trim();
          case "unassigned": return !card.owner?.trim();
          case "labeled": return (card.labels ?? []).length > 0;
          case "unlabeled": return (card.labels ?? []).length === 0;
          case "multi-repo": return (card.repository_ids ?? []).length > 1;
          default: return false;
        }
      }
    }
  });
}

/** Whether every filter in the query holds for the card (words aside). */
export function matchesFilters(card: TaskCard, parsed: ParsedQuery, ctx: RankContext): boolean {
  return parsed.qualifiers.every((q) => qualifierHolds(card, q, ctx) !== q.negated);
}

/** Inverse document frequency over the candidates: a word on every card ranks nothing. */
function idf(total: number, containing: number): number {
  return Math.log(1 + (total - containing + 0.5) / (containing + 0.5));
}

/**
 * The cards matching `parsed`, best first under `relevance` or in the order
 * given under `board`. `cards` is expected in board order; ties keep it.
 */
export function searchCards(cards: readonly TaskCard[], parsed: ParsedQuery, ctx: RankContext, order: SearchOrder = "relevance"): SearchOutcome {
  const filtered: { card: TaskCard; doc: CardDoc; index: number }[] = [];
  cards.forEach((card, index) => {
    if (!matchesFilters(card, parsed, ctx)) return;
    const doc = cardDoc(card, ctx.repoName);
    if (parsed.excluded.some((word) => carries(word, doc))) return;
    if (parsed.excludedPhrases.some((phrase) => containsPhrase(doc, phrase))) return;
    if (!parsed.phrases.every((phrase) => containsPhrase(doc, phrase))) return;
    filtered.push({ card, doc, index });
  });
  if (!parsed.clauses.length) {
    const phraseBonus = parsed.phrases.length ? 1 : 0;
    return { cards: filtered.map(({ card }) => ({ card, score: phraseBonus, matches: [] })), relaxed: false };
  }
  // One pass per clause: which cards match it and how well.
  const perClause = parsed.clauses.map((clause) => {
    const hits = new Map<number, { match: WordMatch; strength: number }>();
    filtered.forEach((entry, i) => {
      let best: { match: WordMatch; strength: number } | null = null;
      for (const word of clause) {
        const hit = matchWord(word, entry.doc, entry.card, ctx);
        if (hit && (!best || hit.strength > best.strength)) best = hit;
      }
      if (best) hits.set(i, best);
    });
    return { hits, weight: idf(filtered.length, hits.size) };
  });
  const full = joinedText(parsed);
  const rank = (needed: number) => {
    const ranked: (RankedCard & { index: number })[] = [];
    filtered.forEach((entry, i) => {
      const matched = perClause.filter((clause) => clause.hits.has(i));
      if (matched.length < needed) return;
      let score = matched.reduce((sum, clause) => sum + clause.weight * clause.hits.get(i)!.strength, 0);
      // Whole query at the start of the title, or anywhere in it: the task named.
      if (full && entry.doc.title.startsWith(full)) score += 2;
      else if (full && entry.doc.title.includes(full)) score += 1;
      score += parsed.phrases.length;
      const matches = matched.map((clause) => clause.hits.get(i)!).sort((a, b) => b.strength - a.strength).map((hit) => hit.match);
      ranked.push({ card: entry.card, score, matches, index: entry.index });
    });
    return ranked;
  };
  let ranked = rank(perClause.length);
  let relaxed = false;
  if (!ranked.length && perClause.length > 1) {
    ranked = rank(1);
    relaxed = ranked.length > 0;
  }
  if (order === "relevance") ranked.sort((a, b) => b.score - a.score || a.index - b.index);
  else ranked.sort((a, b) => a.index - b.index);
  return { cards: ranked.map(({ card, score, matches }) => ({ card, score, matches })), relaxed };
}

function joinedText(parsed: ParsedQuery): string {
  return parsed.clauses.every((clause) => clause.length === 1) ? parsed.clauses.map((clause) => clause[0]).join(" ") : "";
}

const LEVEL_TEXT: Readonly<Record<MatchLevel, string>> = {
  exact: "", prefix: "", stem: "word form", synonym: "related word", substring: "part of a word", typo: "close spelling",
};

/** A short line naming what a ranked card matched on, or "" for exact title hits. */
export function describeMatch(ranked: RankedCard): string {
  const notable = ranked.matches.filter((match) => match.field !== "title" || LEVEL_TEXT[match.level]);
  return notable.slice(0, 2).map((match) => {
    const how = LEVEL_TEXT[match.level];
    const where = match.field === "description" ? "in description" : match.field === "title" ? "" : `in ${match.field}`;
    const shown = match.via && match.via !== match.word ? `“${match.via}”` : "";
    return [`${match.word}`, how && `${how} ${shown}`.trim(), where].filter(Boolean).join(" · ");
  }).join("; ");
}

/** Values of one facet across the matched cards, with how many carry each. */
export interface FacetCount { value: string; count: number }

export function countBy(cards: readonly TaskCard[], pick: (card: TaskCard) => readonly string[]): FacetCount[] {
  const counts = new Map<string, number>();
  for (const card of cards) for (const value of new Set(pick(card))) if (value.trim()) counts.set(value, (counts.get(value) ?? 0) + 1);
  return [...counts].map(([value, count]) => ({ value, count })).sort((a, b) => b.count - a.count || a.value.localeCompare(b.value));
}

/**
 * A dropdown's options with counts: the tasks that would match if this key's
 * filter were set to each value — the rest of the query held, this key's own
 * filter left out, so picking another value is always on offer.
 */
export function facetCounts(cards: readonly TaskCard[], parsed: ParsedQuery, ctx: RankContext, key: QualifierKey, pick: (card: TaskCard) => readonly string[]): FacetCount[] {
  const without: ParsedQuery = { ...parsed, qualifiers: parsed.qualifiers.filter((q) => q.key !== key) };
  return countBy(searchCards(cards, without, ctx, "board").cards.map((ranked) => ranked.card), pick);
}
