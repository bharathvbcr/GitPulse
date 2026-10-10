/**
 * Which cards a search's words match, and in what order.
 *
 * Filters are not decided here: `taskQuery.cardMatchesQuery` (with the
 * board's Filters bar) is the one membership test, and callers rank only the
 * cards it lets through. This module reads the words (`ParsedTaskQuery.words`)
 * and compares each with a card's title, labels, type, owner, severity and
 * repository names at falling strengths — exact, prefix, stem, related word,
 * substring, typo — weighted by how rare the word is among the candidates,
 * so "auth" outranks "fix". Every clause must match; when no card matches
 * them all, the cards matching some are offered instead and the result says
 * so. A phrase must appear in the title or a label.
 *
 * A card does not carry its description. The store's full-text index does,
 * so a word the store already matched for a card (`RankContext.storeMatched`)
 * is credited as a description match even when the card's own fields do not
 * show it.
 */
import type { TaskCard } from "./client";
import { editDistance, fold, stem, synonymsOf, typoBudget, words } from "./taskLexicon";
import type { FreeWords } from "./taskQuery";

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
  repoName: (id: string) => string | undefined;
  /**
   * Whether the store's full-text search already matched this word for this
   * card — in its description, which no card carries. Absent: nothing is
   * credited that the card's own fields do not show.
   */
  storeMatched?: (cardId: string, word: string) => boolean;
}

export interface RankOptions {
  order?: SearchOrder;
  /** Offer cards matching some clauses when none match all. The archive does not: its rows are already the store's answer. */
  relax?: boolean;
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
  /** No card matched every word; these match some of them. */
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
    labels: (card.labels ?? []).map((label) => words(fold(label)).join(" ")),
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
  if (!best && ctx.storeMatched?.(card.id, word)) {
    best = { match: { word, field: "description", level: "prefix", via: word }, strength: LEVEL.prefix * FIELD_WEIGHT.description };
  }
  return best;
}

function containsPhrase(doc: CardDoc, phrase: string): boolean {
  return doc.title.includes(phrase) || doc.labels.some((label) => label.includes(phrase));
}

/** Inverse document frequency over the candidates: a word on every card ranks nothing. */
function idf(total: number, containing: number): number {
  return Math.log(1 + (total - containing + 0.5) / (containing + 0.5));
}

/**
 * The cards matching `free`, best first under `relevance` or in the order
 * given under `board`. `cards` is expected in board order and already through
 * the filters; ties keep the given order.
 */
export function searchCards(cards: readonly TaskCard[], free: FreeWords, ctx: RankContext, options: RankOptions = {}): SearchOutcome {
  const order = options.order ?? "relevance";
  const relax = options.relax ?? true;
  const candidates: { card: TaskCard; doc: CardDoc; index: number }[] = [];
  cards.forEach((card, index) => {
    const doc = cardDoc(card, ctx.repoName);
    if (free.phrases.every((phrase) => containsPhrase(doc, phrase))) candidates.push({ card, doc, index });
  });
  if (!free.clauses.length) {
    const phraseBonus = free.phrases.length ? 1 : 0;
    return { cards: candidates.map(({ card }) => ({ card, score: phraseBonus, matches: [] })), relaxed: false };
  }
  // One pass per clause: which cards match it and how well.
  const perClause = free.clauses.map((clause) => {
    const hits = new Map<number, { match: WordMatch; strength: number }>();
    candidates.forEach((entry, i) => {
      let best: { match: WordMatch; strength: number } | null = null;
      for (const word of clause) {
        const hit = matchWord(word, entry.doc, entry.card, ctx);
        if (hit && (!best || hit.strength > best.strength)) best = hit;
      }
      if (best) hits.set(i, best);
    });
    return { hits, weight: idf(candidates.length, hits.size) };
  });
  const full = joinedText(free);
  const rank = (needed: number) => {
    const ranked: (RankedCard & { index: number })[] = [];
    candidates.forEach((entry, i) => {
      const matched = perClause.flatMap((clause) => {
        const hit = clause.hits.get(i);
        return hit ? [{ hit, weight: clause.weight }] : [];
      });
      if (matched.length < needed) return;
      let score = matched.reduce((sum, { hit, weight }) => sum + weight * hit.strength, 0);
      // Whole query at the start of the title, or anywhere in it: the task named.
      if (full && entry.doc.title.startsWith(full)) score += 2;
      else if (full && entry.doc.title.includes(full)) score += 1;
      score += free.phrases.length;
      const matches = matched.map(({ hit }) => hit).sort((a, b) => b.strength - a.strength).map((hit) => hit.match);
      ranked.push({ card: entry.card, score, matches, index: entry.index });
    });
    return ranked;
  };
  let ranked = rank(perClause.length);
  let relaxed = false;
  if (relax && !ranked.length && perClause.length > 1) {
    ranked = rank(1);
    relaxed = ranked.length > 0;
  }
  if (order === "relevance") ranked.sort((a, b) => b.score - a.score || a.index - b.index);
  else ranked.sort((a, b) => a.index - b.index);
  return { cards: ranked.map(({ card, score, matches }) => ({ card, score, matches })), relaxed };
}

function joinedText(free: FreeWords): string {
  return free.clauses.every((clause) => clause.length === 1) ? free.clauses.map((clause) => clause[0]).join(" ") : "";
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
