/**
 * The task search language — one parser for the Tasks board, the archive
 * dock and (through `repositoryMatches`) the navigator's find box.
 *
 *   login crash                 both words (ranked by `taskRank.ts`)
 *   login OR signin crash       either of the first two, and the third
 *   "sign in"                   that phrase in the title or a label
 *   -flaky  -"wont fix"         titles without these
 *   is:bug  priority:high       filters, matched against each card here
 *   label:ui,ci  -owner:pat     a comma list is any of them; - excludes
 *
 * The store's search (`items.list` `query`) is full-text over a task's title
 * and description, nothing else. Typed into it, `bug` finds tasks that *say*
 * "bug", not tasks whose type is Bug, and `is:bug` finds the same thing — the
 * store splits on the colon and matches both words. So the search box is
 * split before anything reaches the store:
 *
 * - **Filters** (`is:bug`, `priority:high`, `repo:gitpulse`, `-label:ci` …)
 *   are matched here, against each card's fields, by `cardMatchesQuery`.
 * - **Words** are read twice. `text` is what a prefix-AND store can be asked
 *   for: the required words and the phrases, never an `OR` alternative (the
 *   store would AND it) and never a negated word (it would *include* it).
 *   `words` is the same words folded into clauses for local ranking.
 *
 * Within one key, filters widen (`is:bug is:feature` and `is:bug,feature`
 * are either); across keys they narrow (`is:bug owner:pat` is both). A `-`
 * before a filter excludes what it names, and a `-word` with no key excludes
 * cards whose title holds the word.
 *
 * Anything typed that cannot be read (`priority:huge`, a stray `OR`, an
 * unknown `foo:bar`) is reported in `problems`, never quietly dropped or
 * quietly sent to the store as words. Everything here treats the input as
 * hostile — a saved view or a paste — and nothing throws.
 */

import type { Repository, TaskCard, TaskStatus } from "./client";
import { STATUSES, STATUS_LABELS } from "./vocabulary";
import { dueState } from "./taskOrganize";
import { fold, STOPWORDS, words as lexiconWords } from "./taskLexicon";

export type QueryKey = "kind" | "status" | "priority" | "owner" | "label" | "repo" | "due" | "severity" | "title";

/** Every spelling a reader may type for a key, lower case. */
const KEY_ALIASES: Readonly<Record<string, Exclude<QueryKey, "title">>> = {
  is: "kind", type: "kind", kind: "kind",
  status: "status", state: "status", column: "status",
  priority: "priority", prio: "priority", p: "priority",
  owner: "owner", assignee: "owner",
  label: "label", labels: "label", tag: "label",
  repo: "repo", repository: "repo",
  due: "due",
  severity: "severity", sev: "severity",
};

const PRIORITY_NAMES = ["urgent", "high", "normal", "low"] as const;
const DUE_VALUES = ["overdue", "soon", "later", "none", "any"] as const;
type DueValue = (typeof DUE_VALUES)[number];
/** Words that mean "has no value" for owner, label and severity. */
const NONE_WORDS = new Set(["none", "unassigned", "no", "empty"]);

/** The most filters one search applies; the rest are reported, not applied. */
export const MAX_QUERY_TERMS = 32;
/** The store's limit on a search (dc-store `list_items`). */
export const MAX_QUERY_TEXT = 512;

export interface QueryTerm {
  key: QueryKey;
  /** Normalized: lower case, and resolved for status, priority and due. */
  value: string;
  negated: boolean;
  /** As typed, for the chip that shows and removes it. */
  raw: string;
  /** The term's span in the search string. */
  start: number;
  end: number;
}

/** The free words, folded for ranking (`taskRank.ts`). */
export interface FreeWords {
  /** Every clause must match; a clause matches when any of its words does (`a OR b`). */
  clauses: string[][];
  /** Phrases, as folded words joined by one space, that the title or a label must hold. */
  phrases: string[];
}

export interface ParsedTaskQuery {
  /**
   * What a prefix-AND store can search: the required words and the phrases'
   * words, in order. `OR` alternatives are left out — the store would demand
   * all of them — and are checked locally against `words` instead.
   */
  text: string;
  /** Filters the board applies, at most `MAX_QUERY_TERMS`. A comma list is one term per value, sharing its span. */
  terms: QueryTerm[];
  /** What was typed but not applied, each a sentence with the reason, for the reader. */
  problems: string[];
  /** Present only when the search has words or phrases to rank by. */
  words?: FreeWords;
}

export interface QueryContext {
  /** Repositories the board knows by id, for `repo:` by name. */
  repository: (id: string) => Pick<Repository, "name" | "identity_key"> | undefined;
  nowSec: number;
}

export interface Token { text: string; start: number; end: number }

/** Split on whitespace; a double-quoted run (an unclosed one runs to the end) is one token. */
export function queryTokens(input: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < input.length) {
    while (i < input.length && /\s/.test(input[i])) i++;
    if (i >= input.length) break;
    const start = i;
    let quoted = false;
    while (i < input.length && (quoted || !/\s/.test(input[i]))) {
      if (input[i] === '"') quoted = !quoted;
      i++;
    }
    out.push({ text: input.slice(start, i), start, end: i });
  }
  return out;
}

function unquote(value: string): string {
  return value.replace(/"/g, "").replace(/\s+/g, " ").trim();
}

/** A status from what a reader types: `in_progress`, `in-progress`, `In progress`, `wip`. */
function statusValue(value: string): TaskStatus | "open" | null {
  const v = value.toLowerCase().replace(/[\s-]+/g, "_");
  if (v === "open") return "open";
  if (v === "wip" || v === "inprogress" || v === "doing") return "in_progress";
  return STATUSES.find((status) => status === v || STATUS_LABELS[status].toLowerCase().replace(/\s+/g, "_") === v) ?? null;
}

/** `urgent`/`p0`/`0` → "0" … `low`/`p3`/`3` → "3". */
function priorityValue(value: string): string | null {
  const v = value.toLowerCase();
  const named = PRIORITY_NAMES.indexOf(v as (typeof PRIORITY_NAMES)[number]);
  if (named >= 0) return String(named);
  const match = /^p?([0-3])$/.exec(v);
  return match ? match[1] : null;
}

/**
 * GitHub's `is:` words that name a state, not a type. They are read as the
 * board's own filters rather than as a task type nobody has: `is:open` used
 * to empty the board. A custom type spelled like one is still reachable as
 * `type:open`.
 */
const IS_SHORTCUTS: Readonly<Record<string, { key: Exclude<QueryKey, "title">; value: string }>> = {
  open: { key: "status", value: "open" },
  closed: { key: "status", value: "done" },
  done: { key: "status", value: "done" },
  unassigned: { key: "owner", value: "none" },
  overdue: { key: "due", value: "overdue" },
};

function resolve(key: Exclude<QueryKey, "title">, value: string): { value: string } | { problem: string } {
  switch (key) {
    case "status": {
      const status = statusValue(value);
      return status ? { value: status } : { problem: `status:${value} is not a column (${STATUSES.join(", ")}, or open). Not applied.` };
    }
    case "priority": {
      const priority = priorityValue(value);
      return priority ? { value: priority } : { problem: `priority:${value} is not a priority (${PRIORITY_NAMES.join(", ")}, or p0–p3). Not applied.` };
    }
    case "due": {
      const due = DUE_VALUES.find((entry) => entry === value.toLowerCase());
      return due ? { value: due } : { problem: `due:${value} is not a due filter (${DUE_VALUES.join(", ")}). Not applied.` };
    }
    default:
      return { value: value.toLowerCase() };
  }
}

/**
 * A filter's value split at its commas, quotes kept, empty pieces dropped. A
 * comma inside quotes (`owner:"Smith, Jo"`) is part of the value.
 */
function listPieces(value: string): string[] {
  const pieces: string[] = [];
  let piece = "", quoted = false;
  for (const ch of value) {
    if (ch === '"') quoted = !quoted;
    if (ch === "," && !quoted) { pieces.push(piece); piece = ""; } else piece += ch;
  }
  pieces.push(piece);
  return pieces.filter((entry) => unquote(entry) !== "");
}

/** `OR` (or `|`) between two words makes them alternatives; lower-case `or` is a word. */
function isOr(token: string): boolean {
  return token === "OR" || token === "|";
}

/** A word token: `raws` holds one entry, or several joined by `OR`. */
interface WordGroup { raws: string[]; words: string[][] }

/** The folded clauses for the groups, stopwords aside unless they are all there is. */
function clausesOf(groups: readonly WordGroup[], phrases: readonly string[]): string[][] {
  const clauses: string[][] = [];
  const stopped: string[] = [];
  const keep = (list: readonly string[]) => {
    stopped.push(...list.filter((word) => STOPWORDS.has(word)));
    return list.filter((word) => !STOPWORDS.has(word));
  };
  for (const group of groups) {
    const kept = group.words.map(keep);
    if (group.raws.length === 1) {
      for (const word of kept[0]) clauses.push([word]);
      continue;
    }
    // `a OR b-c`: each alternative is its token's first word; a token's
    // other words must match as well, as they would without the OR.
    const alternatives = [...new Set(kept.map((list) => list[0]).filter((word): word is string => Boolean(word)))];
    if (alternatives.length) clauses.push(alternatives);
    for (const list of kept) for (const word of list.slice(1)) clauses.push([word]);
  }
  // Stopwords say nothing beside other words, but a search of only
  // stopwords ("the", "todo") is still a search for them.
  if (!clauses.length && !phrases.length && stopped.length) return [...new Set(stopped)].map((word) => [word]);
  return clauses;
}

/** Read a search string into store text, board filters and ranking words. */
export function parseTaskQuery(input: unknown): ParsedTaskQuery {
  const source = typeof input === "string" ? input.slice(0, MAX_QUERY_TEXT) : "";
  const terms: QueryTerm[] = [];
  const problems: string[] = [];
  const groups: WordGroup[] = [];
  const phrases: string[] = [];
  /** Store text in typed order: a phrase as typed, or the group it belongs to. */
  const textOrder: (string | WordGroup)[] = [];
  let dropped = 0;
  /** An `OR` is waiting for the word after it. */
  let joinNext = false;
  /** The token before was a word, so an `OR` after it has something to join. */
  let afterWord = false;
  const add = (term: QueryTerm) => {
    if (terms.length < MAX_QUERY_TERMS) terms.push(term);
    else dropped++;
  };
  const orRefused = () => {
    if (joinNext) problems.push("OR joins two words; the one here has no word after it, so it was ignored.");
    joinNext = false;
  };
  for (const token of queryTokens(source)) {
    if (isOr(token.text)) {
      if (!afterWord || joinNext) problems.push("OR joins two words; the one here has no word before it, so it was ignored.");
      else joinNext = true;
      afterWord = false;
      continue;
    }
    const negated = token.text.length > 1 && token.text.startsWith("-");
    const body = negated ? token.text.slice(1) : token.text;
    const qualifier = /^([A-Za-z]+):(.*)$/s.exec(body);
    const key = qualifier ? KEY_ALIASES[qualifier[1].toLowerCase()] : undefined;
    if (qualifier && key) {
      orRefused();
      afterWord = false;
      const seen = new Set<string>();
      // `is:` alone is a filter being typed, not words to search for.
      for (const piece of listPieces(qualifier[2])) {
        const value = unquote(piece);
        const shortcut = qualifier[1].toLowerCase() === "is" ? IS_SHORTCUTS[value.toLowerCase()] : undefined;
        const target = shortcut?.key ?? key;
        const resolved = resolve(target, shortcut?.value ?? value);
        if ("problem" in resolved) { problems.push(resolved.problem); continue; }
        if (seen.has(`${target}:${resolved.value}`)) continue;
        seen.add(`${target}:${resolved.value}`);
        add({ key: target, value: resolved.value, negated, raw: token.text, start: token.start, end: token.end });
      }
      continue;
    }
    if (negated) {
      orRefused();
      afterWord = false;
      const word = unquote(body).toLowerCase();
      if (word) add({ key: "title", value: word, negated: true, raw: token.text, start: token.start, end: token.end });
      continue;
    }
    // The store searches only letters, digits and `_`; a token with none of
    // them becomes an empty match there and would empty the whole board.
    const word = unquote(token.text);
    if (!/[\p{L}\p{N}_]/u.test(word)) continue;
    if (token.text.startsWith('"')) {
      orRefused();
      afterWord = false;
      textOrder.push(word);
      const phrase = lexiconWords(word).join(" ");
      if (phrase) phrases.push(phrase);
      continue;
    }
    // `https://…` and `10:30` are words; `foo:bar` reads like a filter that is not one.
    if (qualifier && /^[a-z]{2,12}$/i.test(qualifier[1]) && qualifier[2] !== "" && !qualifier[2].startsWith("//")) {
      problems.push(`${qualifier[1]}: is not a filter, so "${word}" was searched as words.`);
    }
    const list = lexiconWords(word);
    if (joinNext && groups.length) {
      const last = groups[groups.length - 1];
      last.raws.push(word);
      last.words.push(list);
    } else {
      const group: WordGroup = { raws: [word], words: [list] };
      groups.push(group);
      textOrder.push(group);
    }
    joinNext = false;
    afterWord = true;
  }
  if (joinNext) problems.push("OR joins two words; the one at the end has no word after it, so it was ignored.");
  if (dropped > 0) problems.push(`Only the first ${MAX_QUERY_TERMS} filters apply; ${dropped} more ${dropped === 1 ? "was" : "were"} ignored.`);
  const text = textOrder
    .flatMap((entry) => (typeof entry === "string" ? [entry] : entry.raws.length === 1 ? entry.raws : []))
    .join(" ")
    .slice(0, MAX_QUERY_TEXT);
  const clauses = clausesOf(groups, phrases);
  const parsed: ParsedTaskQuery = { text, terms, problems: [...new Set(problems)] };
  if (clauses.length || phrases.length) parsed.words = { clauses, phrases };
  return parsed;
}

/** Nothing to search or filter by: the board reads as it does without a search. */
export function isEmptyQuery(parsed: ParsedTaskQuery): boolean {
  return parsed.terms.length === 0 && !parsed.words;
}

/** A plural kind (`bugs`) reads as its singular when no kind is spelled that way. */
function kindMatches(kind: string, value: string): boolean {
  const k = kind.trim().toLowerCase();
  return k === value || (value.length > 1 && value.endsWith("s") && k === value.slice(0, -1));
}

/**
 * Whether a repository answers to what a reader typed: part of its name, a
 * whole part of its identity (a directory, an owner, a host), or — once the
 * text holds a `/` — any stretch of the identity. Not any stretch without one:
 * every local identity ends in `/.git` and most sit under `/Users/<name>`, so
 * a plain substring made `git` or `users` match every repository.
 *
 * The one test for both the board's `repo:` filter and the navigator's box,
 * so the two cannot disagree about which repository a word names.
 */
export function repositoryMatches(repo: Pick<Repository, "name" | "identity_key">, needle: string): boolean {
  const value = needle.trim().toLowerCase();
  if (!value) return true;
  if (repo.name.toLowerCase().includes(value)) return true;
  const identity = repo.identity_key.toLowerCase();
  if (value.includes("/")) return identity.includes(value);
  return identity.split(/[/:@\\]+/).some((part) => part === value || part.replace(/\.git$/, "") === value);
}

function repoMatches(card: TaskCard, value: string, ctx: QueryContext): boolean {
  return card.repository_ids.some((id) => {
    if (id.toLowerCase() === value) return true;
    const repo = ctx.repository(id);
    return repo ? repositoryMatches(repo, value) : false;
  });
}

function dueMatches(card: TaskCard, value: string, nowSec: number): boolean {
  const state = dueState(card.due_at, nowSec);
  if ((value as DueValue) === "any") return state !== "none";
  return state === value;
}

/** Whether one filter, read positively, holds for the card. */
function termHolds(card: TaskCard, term: QueryTerm, ctx: QueryContext): boolean {
  const none = NONE_WORDS.has(term.value);
  switch (term.key) {
    case "kind": return kindMatches(card.kind, term.value);
    case "status": return term.value === "open" ? card.status !== "done" : card.status === term.value;
    case "priority": return String(card.priority) === term.value;
    case "owner": {
      const owner = (card.owner ?? "").trim().toLowerCase();
      return none ? owner === "" : owner === term.value;
    }
    case "label": {
      const labels = card.labels.map((label) => label.trim().toLowerCase()).filter(Boolean);
      return none ? labels.length === 0 : labels.includes(term.value);
    }
    case "severity": {
      const severity = (card.severity ?? "").trim().toLowerCase();
      return none ? severity === "" : severity === term.value;
    }
    case "repo": return repoMatches(card, term.value, ctx);
    case "due": return dueMatches(card, term.value, ctx.nowSec);
    case "title": return card.title.toLowerCase().includes(term.value);
  }
}

/**
 * Whether a card passes every filter.
 *
 * Positive filters on one key are alternatives; each key that has any must
 * be met by one of them. Every negated filter must fail.
 */
export function cardMatchesQuery(card: TaskCard, parsed: ParsedTaskQuery, ctx: QueryContext): boolean {
  if (parsed.terms.length === 0) return true;
  const wanted = new Map<QueryKey, boolean>();
  for (const term of parsed.terms) {
    const holds = termHolds(card, term, ctx);
    if (term.negated) { if (holds) return false; continue; }
    wanted.set(term.key, (wanted.get(term.key) ?? false) || holds);
  }
  for (const met of wanted.values()) if (!met) return false;
  return true;
}

type Span = Pick<QueryTerm, "start" | "end" | "raw">;

/**
 * Replace spans of the search (with "" to take one out), then tidy the
 * spacing once. Spans are edited right to left on the untouched string, so
 * each one is still where it was parsed; a span that no longer holds its
 * term (the search changed since) is left alone rather than cutting someone
 * else's text. One span is edited once, however many terms share it.
 */
function editSpans(search: string, edits: readonly (Span & { replacement: string })[]): string {
  let next = search;
  const seen = new Set<number>();
  for (const edit of [...edits].sort((a, b) => b.start - a.start)) {
    if (seen.has(edit.start) || next.slice(edit.start, edit.end) !== edit.raw) continue;
    seen.add(edit.start);
    next = `${next.slice(0, edit.start)} ${edit.replacement} ${next.slice(edit.end)}`;
  }
  return next === search ? search : next.replace(/\s+/g, " ").trim();
}

/** The search string with one filter taken out, as its chip's remove button does: the whole token, every value of a list. */
export function removeQueryTerm(search: string, term: Span): string {
  return editSpans(search, [{ ...term, replacement: "" }]);
}

function sameTerm(a: QueryTerm, b: QueryTerm): boolean {
  return a.key === b.key && a.value === b.value && a.negated === b.negated;
}

/** Whether the search already applies this filter (`is:bug`), however it was spelled. */
export function queryHas(parsed: ParsedTaskQuery, filter: string): boolean {
  const [wanted] = parseTaskQuery(filter).terms;
  return Boolean(wanted) && parsed.terms.some((term) => sameTerm(term, wanted));
}

/**
 * A list token (`is:bug,feature`) without the values that read as `wanted`,
 * or "" when none would be left. Values that do not read as it — unreadable
 * ones included — stay as typed.
 */
function tokenWithout(raw: string, wanted: QueryTerm): string {
  const match = /^(-?)([A-Za-z]+):(.*)$/s.exec(raw);
  if (!match) return "";
  const kept = listPieces(match[3]).filter((piece) => {
    const [term] = parseTaskQuery(`${match[1]}${match[2]}:${piece}`).terms;
    return !term || !sameTerm(term, wanted);
  });
  return kept.length ? `${match[1]}${match[2]}:${kept.join(",")}` : "";
}

/**
 * The search with a filter switched: every spelling of it taken out when the
 * search has it (just that value, from a comma list), otherwise appended. A
 * filter that does not parse, or one that would push the search past the
 * store's limit, leaves it unchanged.
 */
export function toggleQueryTerm(search: string, filter: string): string {
  const [wanted] = parseTaskQuery(filter).terms;
  if (!wanted) return search;
  const present = parseTaskQuery(search).terms.filter((term) => sameTerm(term, wanted));
  if (present.length > 0) return editSpans(search, present.map((term) => ({ ...term, replacement: tokenWithout(term.raw, wanted) })));
  const next = search.trim() ? `${search.trim()} ${filter.trim()}` : filter.trim();
  return next.length <= MAX_QUERY_TEXT ? next : search;
}

/** One chip per filter token: a comma list is one chip, removed whole. */
export function queryChips(parsed: ParsedTaskQuery): QueryTerm[] {
  const seen = new Set<number>();
  return parsed.terms.filter((term) => !seen.has(term.start) && Boolean(seen.add(term.start)));
}

// ---- Completions -----------------------------------------------------------

/** One completion row: what it shows and the token it puts in place of the one being typed. */
export interface QuerySuggestion { label: string; detail: string; insert: string }

/** Values the loaded board can offer per key; fixed vocabularies are offered whatever loaded. */
export type SuggestionSources = Partial<Record<"kind" | "owner" | "label" | "severity" | "repo", readonly string[]>>;

export const MAX_SUGGESTIONS = 8;

/** The keys a reader is offered, in the spelling the help uses. */
const SUGGESTED_KEYS: readonly { name: string; detail: string }[] = [
  { name: "is", detail: "a type, or open, closed, unassigned, overdue" },
  { name: "type", detail: "a task type" },
  { name: "status", detail: "a column, or open" },
  { name: "priority", detail: "urgent, high, normal, low" },
  { name: "owner", detail: "an owner, or none" },
  { name: "label", detail: "a label, or none" },
  { name: "repo", detail: "a linked repository" },
  { name: "due", detail: "overdue, soon, later, none, any" },
  { name: "severity", detail: "a severity, or none" },
];

/** A value as a token carries it: quoted when it holds a space, a comma or nothing. */
export function quoteValue(value: string): string {
  const clean = value.replace(/"/g, "").trim();
  return /[\s,]/.test(clean) || clean === "" ? `"${clean}"` : clean;
}

function valuesFor(name: string, sources: SuggestionSources): readonly string[] {
  const key = KEY_ALIASES[name.toLowerCase()];
  switch (key) {
    case "kind": return name.toLowerCase() === "is" ? [...Object.keys(IS_SHORTCUTS), ...(sources.kind ?? [])] : sources.kind ?? [];
    case "status": return [...STATUSES, "open"];
    case "priority": return PRIORITY_NAMES;
    case "due": return DUE_VALUES;
    case "owner": return [...(sources.owner ?? []), "none"];
    case "label": return [...(sources.label ?? []), "none"];
    case "severity": return [...(sources.severity ?? []), "none"];
    case "repo": return sources.repo ?? [];
    default: return [];
  }
}

/**
 * Completions for the token being typed: key names once two letters match
 * one, then that key's values after the last comma of a list. Never throws;
 * an unknown key offers nothing.
 */
export function suggest(prefix: string, sources: SuggestionSources): QuerySuggestion[] {
  const match = /^(-?)([A-Za-z]+):(.*)$/s.exec(prefix);
  if (match) {
    if (!KEY_ALIASES[match[2].toLowerCase()]) return [];
    const rest = match[3];
    const quotedTail = rest.startsWith('"');
    const comma = quotedTail ? -1 : rest.lastIndexOf(",");
    const head = rest.slice(0, comma + 1);
    const partial = fold(unquote(rest.slice(comma + 1)));
    const values = [...new Set(valuesFor(match[2], sources).filter((value) => value.trim()))];
    const starts = values.filter((value) => fold(value).startsWith(partial));
    const contains = values.filter((value) => !fold(value).startsWith(partial) && fold(value).includes(partial));
    return [...starts, ...contains]
      .filter((value) => fold(value) !== partial || partial === "")
      .slice(0, MAX_SUGGESTIONS)
      .map((value) => ({ label: `${match[2]}:${value}`, detail: "", insert: `${match[1]}${match[2]}:${head}${quoteValue(value)}` }));
  }
  const word = fold(prefix.replace(/^-/, ""));
  if (word.length < 2 || !/^[a-z]+$/.test(word)) return [];
  return SUGGESTED_KEYS.filter((key) => key.name.startsWith(word) && key.name !== word)
    .map((key) => ({ label: `${key.name}:`, detail: key.detail, insert: `${prefix.startsWith("-") ? "-" : ""}${key.name}:` }));
}

/** The token the caret is in: its index and the text up to the caret. */
export function tokenAtCaret(text: string, caret: number): { index: number; prefix: string } | null {
  const list = queryTokens(typeof text === "string" ? text.slice(0, MAX_QUERY_TEXT) : "");
  const index = list.findIndex((token) => caret >= token.start && caret <= token.end);
  if (index === -1) return null;
  return { index, prefix: text.slice(list[index].start, caret) };
}

/**
 * `text` with the token at `index` replaced by `raw`, and where the caret
 * goes: after a space to keep typing, or straight after a bare `key:`.
 */
export function replaceToken(text: string, index: number, raw: string): { text: string; caret: number } {
  const list = queryTokens(text.slice(0, MAX_QUERY_TEXT)).map((token) => token.text);
  if (index < 0 || index >= list.length) return { text, caret: text.length };
  list[index] = raw;
  const before = list.slice(0, index + 1).join(" ");
  const after = list.slice(index + 1).join(" ");
  const gap = raw.endsWith(":") ? "" : " ";
  const joined = `${before}${gap}${after ? (gap ? "" : " ") + after : ""}`;
  return { text: joined.slice(0, MAX_QUERY_TEXT), caret: Math.min(before.length + gap.length, MAX_QUERY_TEXT) };
}

/** The filters a reader can type, for the search box's help. */
export const QUERY_HELP: readonly { example: string; meaning: string }[] = [
  { example: "is:bug", meaning: "Type (also type:, kind:; is:bugs works)" },
  { example: "is:open", meaning: "Also is:closed, is:unassigned, is:overdue" },
  { example: "priority:high", meaning: "urgent, high, normal, low, or p0–p3" },
  { example: "status:review", meaning: "A column, or status:open for not done" },
  { example: "repo:gitpulse", meaning: "A linked repository: part of its name, a folder or owner, or a path with /" },
  { example: "owner:pat", meaning: "Owner; owner:none for unassigned" },
  { example: "label:ci", meaning: "Label; label:none for unlabelled" },
  { example: "severity:high", meaning: "Severity; severity:none for unset" },
  { example: "due:overdue", meaning: "overdue, soon, later, none, any" },
  { example: "-is:bug", meaning: "A leading - excludes; -word excludes titles holding it" },
  { example: "label:ui,ci", meaning: "A comma list is any of its values" },
  { example: 'label:"needs review"', meaning: "Quote a value with spaces" },
];
