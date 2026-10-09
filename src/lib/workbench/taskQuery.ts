/**
 * The Tasks board's search language: free text plus `key:value` filters.
 *
 * The store's search (`items.list` `query`) is full-text over a task's title
 * and description, nothing else. Typed into it, `bug` finds tasks that *say*
 * "bug", not tasks whose type is Bug, and `is:bug` finds the same thing — the
 * store splits on the colon and matches both words. So the board splits the
 * search box in two before anything reaches the store:
 *
 * - **Filters** (`is:bug`, `priority:high`, `repo:gitpulse`, `-label:ci` …)
 *   are matched here, against each card's fields, the way the Filters bar
 *   already matches its facets.
 * - **Text** — everything else — is what the store searches, unchanged.
 *
 * Like the facets, filters see the pages the board has loaded; a column's
 * "Load more" brings the rest under them. The board says so where it reports
 * a filtered count.
 *
 * Within one key, filters widen (`is:bug is:feature` is either); across keys
 * they narrow (`is:bug owner:pat` is both). A `-` before a filter excludes
 * what it names, and a `-word` with no key excludes cards whose title holds
 * the word — the store has no NOT, so a negated word must not reach it,
 * where it would *include* the word instead.
 *
 * A filter the board cannot read (`priority:huge`) is reported, never quietly
 * dropped into the text: sent to the store it would search for the words.
 * Everything here treats the input as hostile — a saved view or a paste — and
 * nothing throws.
 */

import type { Repository, TaskCard, TaskStatus } from "./client";
import { STATUSES, STATUS_LABELS } from "./vocabulary";
import { dueState } from "./taskOrganize";

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

export interface ParsedTaskQuery {
  /** What the store searches: the free words, in order. */
  text: string;
  /** Filters the board applies, at most `MAX_QUERY_TERMS`. */
  terms: QueryTerm[];
  /** Filters typed but not applied, each with the reason, for the reader. */
  problems: string[];
}

export interface QueryContext {
  /** Repositories the board knows by id, for `repo:` by name. */
  repository: (id: string) => Pick<Repository, "name" | "identity_key"> | undefined;
  nowSec: number;
}

interface Token { text: string; start: number; end: number }

/** Split on whitespace; a double-quoted run (an unclosed one runs to the end) is one token. */
function tokens(input: string): Token[] {
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
      return status ? { value: status } : { problem: `status:${value} is not a column (${STATUSES.join(", ")}, or open).` };
    }
    case "priority": {
      const priority = priorityValue(value);
      return priority ? { value: priority } : { problem: `priority:${value} is not a priority (${PRIORITY_NAMES.join(", ")}, or p0–p3).` };
    }
    case "due": {
      const due = DUE_VALUES.find((entry) => entry === value.toLowerCase());
      return due ? { value: due } : { problem: `due:${value} is not a due filter (${DUE_VALUES.join(", ")}).` };
    }
    default:
      return { value: value.toLowerCase() };
  }
}

/** Read a search string into store text and board filters. */
export function parseTaskQuery(input: unknown): ParsedTaskQuery {
  const source = typeof input === "string" ? input.slice(0, MAX_QUERY_TEXT) : "";
  const text: string[] = [];
  const terms: QueryTerm[] = [];
  const problems: string[] = [];
  let dropped = 0;
  const add = (term: QueryTerm) => {
    if (terms.length < MAX_QUERY_TERMS) terms.push(term);
    else dropped++;
  };
  for (const token of tokens(source)) {
    const negated = token.text.length > 1 && token.text.startsWith("-");
    const body = negated ? token.text.slice(1) : token.text;
    const qualifier = /^([A-Za-z]+):(.*)$/s.exec(body);
    const key = qualifier ? KEY_ALIASES[qualifier[1].toLowerCase()] : undefined;
    if (qualifier && key) {
      const value = unquote(qualifier[2]);
      // `is:` alone is a filter being typed, not words to search for.
      if (!value) continue;
      const shortcut = qualifier[1].toLowerCase() === "is" ? IS_SHORTCUTS[value.toLowerCase()] : undefined;
      const target = shortcut?.key ?? key;
      const resolved = resolve(target, shortcut?.value ?? value);
      if ("problem" in resolved) { problems.push(resolved.problem); continue; }
      add({ key: target, value: resolved.value, negated, raw: token.text, start: token.start, end: token.end });
      continue;
    }
    if (negated) {
      const word = unquote(body).toLowerCase();
      if (word) add({ key: "title", value: word, negated: true, raw: token.text, start: token.start, end: token.end });
      continue;
    }
    // The store searches only letters, digits and `_`; a token with none of
    // them becomes an empty match there and would empty the whole board.
    const word = unquote(token.text);
    if (/[\p{L}\p{N}_]/u.test(word)) text.push(word);
  }
  if (dropped > 0) problems.push(`Only the first ${MAX_QUERY_TERMS} filters apply; ${dropped} more ${dropped === 1 ? "was" : "were"} ignored.`);
  return { text: text.join(" ").slice(0, MAX_QUERY_TEXT), terms, problems };
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

/**
 * Take spans out of the search, then tidy the spacing once. Spans are cut
 * right to left on the untouched string, so each one is still where it was
 * parsed; a span that no longer holds its term (the search changed since) is
 * left alone rather than cutting someone else's text.
 */
function cutTerms(search: string, terms: readonly Pick<QueryTerm, "start" | "end" | "raw">[]): string {
  let next = search;
  for (const term of [...terms].sort((a, b) => b.start - a.start)) {
    if (next.slice(term.start, term.end) !== term.raw) continue;
    next = `${next.slice(0, term.start)} ${next.slice(term.end)}`;
  }
  return next === search ? search : next.replace(/\s+/g, " ").trim();
}

/** The search string with one filter taken out, as its chip's remove button does. */
export function removeQueryTerm(search: string, term: Pick<QueryTerm, "start" | "end" | "raw">): string {
  return cutTerms(search, [term]);
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
 * The search with a filter switched: every spelling of it taken out when the
 * search has it, otherwise appended. A filter that does not parse, or one
 * that would push the search past the store's limit, leaves it unchanged.
 */
export function toggleQueryTerm(search: string, filter: string): string {
  const [wanted] = parseTaskQuery(filter).terms;
  if (!wanted) return search;
  const present = parseTaskQuery(search).terms.filter((term) => sameTerm(term, wanted));
  if (present.length > 0) return cutTerms(search, present);
  const next = search.trim() ? `${search.trim()} ${filter.trim()}` : filter.trim();
  return next.length <= MAX_QUERY_TEXT ? next : search;
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
  { example: 'label:"needs review"', meaning: "Quote a value with spaces" },
];
