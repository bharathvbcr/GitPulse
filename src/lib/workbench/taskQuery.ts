/**
 * The task board's search language: free words, quoted phrases, `OR`, and
 * `key:value` filters.
 *
 *   login crash                 both ideas (each through synonyms, word forms, typos)
 *   login OR signin crash       either of the first two, and the third
 *   "sign in"                   that phrase in the title or a label
 *   -flaky  -"wont fix"         without these
 *   repo:gitpulse,manvi         in either repository
 *   label:ui label:a11y         carrying both labels
 *   -owner:@pat is:overdue      not Pat's, past due
 *   priority:urgent,high  kind:bug  status:review  severity:high
 *
 * The parsed query is the one source of truth for the board's filters: the
 * Filters dropdowns write into the text through `setQualifier` and read back
 * through `qualifierState`, so a saved view is one string and the dropdowns
 * can never disagree with what is typed.
 */
import { STATUSES, type TaskStatus } from "./vocabulary";
import { emptyFacet, normalizePriority, type TaskFacet } from "./taskOrganize";
import { fold, stem, STOPWORDS, words } from "./taskLexicon";

export const MAX_QUERY = 512;
/** The most parts one query is read with; the rest is reported, not dropped silently. */
export const MAX_QUERY_PARTS = 32;
/** A qualifier value as long as a facet value may be (`taskBoardViews.ts`). */
export const MAX_QUALIFIER_VALUE = 300;

export const QUALIFIER_KEYS = ["repo", "label", "owner", "kind", "status", "priority", "severity", "is"] as const;
export type QualifierKey = (typeof QUALIFIER_KEYS)[number];

const ALIASES: Readonly<Record<string, QualifierKey>> = {
  repo: "repo", repos: "repo", repository: "repo",
  label: "label", labels: "label", tag: "label",
  owner: "owner", assignee: "owner", by: "owner",
  kind: "kind", type: "kind",
  status: "status", column: "status", col: "status",
  priority: "priority", prio: "priority", p: "priority",
  severity: "severity", sev: "severity",
  is: "is", has: "is",
};

/** `is:` states. `has:due` and `has:owner` read as their `is:` twins. */
export const IS_VALUES = ["overdue", "soon", "due", "no-due", "assigned", "unassigned", "labeled", "unlabeled", "multi-repo"] as const;
export type IsValue = (typeof IS_VALUES)[number];
const IS_ALIASES: Readonly<Record<string, IsValue>> = {
  overdue: "overdue", late: "overdue", "past-due": "overdue",
  soon: "soon", "due-soon": "soon", duesoon: "soon",
  due: "due", "has-due": "due", dated: "due",
  "no-due": "no-due", nodue: "no-due", undated: "no-due",
  assigned: "assigned", owner: "assigned", owned: "assigned",
  unassigned: "unassigned", unowned: "unassigned",
  labeled: "labeled", labelled: "labeled", label: "labeled", labels: "labeled",
  unlabeled: "unlabeled", unlabelled: "unlabeled",
  "multi-repo": "multi-repo", multirepo: "multi-repo", shared: "multi-repo",
};

const STATUS_ALIASES: Readonly<Record<string, TaskStatus>> = {
  inbox: "inbox", new: "inbox", triage: "inbox",
  backlog: "backlog", later: "backlog",
  ready: "ready", todo: "ready", next: "ready",
  in_progress: "in_progress", "in-progress": "in_progress", inprogress: "in_progress", progress: "in_progress", doing: "in_progress", wip: "in_progress", active: "in_progress",
  review: "review", reviewing: "review", "in-review": "review",
  done: "done", closed: "done", complete: "done", completed: "done", finished: "done",
};

const PRIORITY_ALIASES: Readonly<Record<string, number>> = {
  urgent: 0, critical: 0, p0: 0, "0": 0,
  high: 1, p1: 1, "1": 1,
  normal: 2, medium: 2, p2: 2, "2": 2,
  low: 3, p3: 3, "3": 3,
};
export const PRIORITY_NAMES = ["urgent", "high", "normal", "low"] as const;

export interface Qualifier {
  key: QualifierKey;
  /** Any of these (a comma list); normalized per key. Never empty. */
  values: string[];
  negated: boolean;
}

export interface ParsedQuery {
  /** Every clause must match; a clause matches when any of its words does. */
  clauses: string[][];
  /** Words a match must not carry. */
  excluded: string[];
  /** Folded phrases the title or a label must contain. */
  phrases: string[];
  excludedPhrases: string[];
  qualifiers: Qualifier[];
  /** What was not understood, said once each, for the line under the search box. */
  warnings: string[];
}

/** One whitespace-separated part of the text, quotes kept together. */
export interface QueryPart { raw: string; start: number; end: number }

/** The parts of `text` as typed: a quoted run is one part, an unclosed quote runs to the end. */
export function splitQuery(text: string): QueryPart[] {
  const parts: QueryPart[] = [];
  const source = typeof text === "string" ? text.slice(0, MAX_QUERY) : "";
  let i = 0;
  while (i < source.length) {
    while (i < source.length && /\s/.test(source[i])) i++;
    if (i >= source.length) break;
    const start = i;
    let quoted = false;
    while (i < source.length && (quoted || !/\s/.test(source[i]))) {
      if (source[i] === '"') quoted = !quoted;
      i++;
    }
    parts.push({ raw: source.slice(start, i), start, end: i });
  }
  return parts;
}

function unquote(value: string): string {
  return value.replace(/^"/, "").replace(/"$/, "");
}

/** A value as the text carries it: quoted when it has a space, a comma or a quote. */
export function quoteValue(value: string): string {
  const clean = value.replace(/"/g, "").trim();
  return /[\s,]/.test(clean) || clean === "" ? `"${clean}"` : clean;
}

/** `key:value` read as a qualifier, or null when the part is not one. */
export function readQualifier(raw: string): { key: QualifierKey | null; name: string; negated: boolean; values: string[] } | null {
  const match = /^(-?)([A-Za-z_]+):(.*)$/s.exec(raw);
  if (!match) return null;
  const name = match[2].toLowerCase();
  const rest = match[3];
  const values = rest.startsWith('"')
    ? [unquote(rest)]
    : rest.split(",").map((value) => unquote(value));
  return {
    key: ALIASES[name] ?? null,
    name,
    negated: match[1] === "-",
    values: values.map((value) => value.trim()).filter(Boolean).map((value) => value.slice(0, MAX_QUALIFIER_VALUE)),
  };
}

function normalizeValue(key: QualifierKey, value: string, warnings: Set<string>): string | null {
  const folded = fold(value).trim();
  switch (key) {
    case "status": {
      const status = STATUS_ALIASES[folded.replace(/\s+/g, "-")] ?? STATUS_ALIASES[folded.replace(/[\s-]+/g, "_")];
      if (!status) warnings.add(`Unknown status "${value}" — use ${STATUSES.join(", ")}.`);
      return status ?? null;
    }
    case "priority": {
      const priority = PRIORITY_ALIASES[folded];
      if (priority === undefined) warnings.add(`Unknown priority "${value}" — use urgent, high, normal or low.`);
      return priority === undefined ? null : String(priority);
    }
    case "is": {
      const state = IS_ALIASES[folded.replace(/[\s_]+/g, "-")];
      if (!state) warnings.add(`Unknown state "is:${value}" — use ${IS_VALUES.join(", ")}.`);
      return state ?? null;
    }
    default:
      return value;
  }
}

/** The query in `text`, every part read once; nothing in it throws. */
export function parseTaskQuery(text: string): ParsedQuery {
  const parsed: ParsedQuery = { clauses: [], excluded: [], phrases: [], excludedPhrases: [], qualifiers: [], warnings: [] };
  const warnings = new Set<string>();
  const all = splitQuery(text);
  if (typeof text === "string" && text.length > MAX_QUERY) warnings.add(`Only the first ${MAX_QUERY} characters are searched.`);
  if (all.length > MAX_QUERY_PARTS) warnings.add(`Only the first ${MAX_QUERY_PARTS} search terms are used.`);
  let joinNext = false;
  // Stopwords say nothing beside other words, but a query of only stopwords
  // ("todo", "the") is still a search for them.
  const stopped: string[] = [];
  const addWords = (list: string[]) => {
    const kept = list.filter((word) => !STOPWORDS.has(word));
    stopped.push(...list.filter((word) => STOPWORDS.has(word)));
    if (!kept.length) return;
    if (joinNext && parsed.clauses.length) {
      // `a OR b-c`: the alternative is its first word; the rest must also match.
      parsed.clauses[parsed.clauses.length - 1].push(kept[0]);
      for (const word of kept.slice(1)) parsed.clauses.push([word]);
    } else for (const word of kept) parsed.clauses.push([word]);
    joinNext = false;
  };
  for (const { raw } of all.slice(0, MAX_QUERY_PARTS)) {
    if (raw === "OR" || raw === "|") {
      if (parsed.clauses.length) joinNext = true;
      continue;
    }
    const qualifier = readQualifier(raw);
    if (qualifier?.key) {
      const values = qualifier.values.map((value) => normalizeValue(qualifier.key!, value, warnings)).filter((value): value is string => value !== null);
      if (values.length) parsed.qualifiers.push({ key: qualifier.key, values: [...new Set(values)], negated: qualifier.negated });
      else if (!qualifier.values.length) warnings.add(`"${raw}" has no value, so it filters nothing.`);
      joinNext = false;
      continue;
    }
    if (qualifier && /^[a-z]{2,12}$/.test(qualifier.name) && !qualifier.values.some((value) => value.startsWith("//"))) warnings.add(`"${qualifier.name}:" is not a filter; searched as words.`);
    const negated = raw.startsWith("-") && raw.length > 1;
    const body = negated ? raw.slice(1) : raw;
    if (body.startsWith('"')) {
      const phrase = words(unquote(body)).join(" ");
      if (phrase) (negated ? parsed.excludedPhrases : parsed.phrases).push(phrase);
      joinNext = false;
      continue;
    }
    const list = words(body);
    if (negated) parsed.excluded.push(...list.filter((word) => !STOPWORDS.has(word)));
    else addWords(list);
  }
  if (!parsed.clauses.length && !parsed.phrases.length && stopped.length) parsed.clauses = [...new Set(stopped)].map((word) => [word]);
  parsed.warnings = [...warnings];
  return parsed;
}

/** Nothing to search or filter by: the board reads as it does without a query. */
export function isEmptyQuery(parsed: ParsedQuery): boolean {
  return !parsed.clauses.length && !parsed.excluded.length && !parsed.phrases.length && !parsed.excludedPhrases.length && !parsed.qualifiers.length;
}

/** Whether the query has words to rank by, which is when Relevance means anything. */
export function hasFreeText(parsed: ParsedQuery): boolean {
  return parsed.clauses.length > 0 || parsed.phrases.length > 0;
}

/**
 * The one word the store is asked for, so its full-text index can find tasks
 * whose *description* says it — the board's cards do not carry descriptions.
 *
 * The store ANDs every word it is given as a prefix, so sending them all only
 * narrows; the longest required word is the most selective. It goes as the
 * common prefix of the word and its stem, so "dependencies" still finds
 * "dependency". Null when there is no word worth a request.
 */
export function serverWord(parsed: ParsedQuery): string | null {
  const required = parsed.clauses.filter((clause) => clause.length === 1).map((clause) => clause[0]);
  const candidates = required.length ? required : parsed.phrases.flatMap((phrase) => phrase.split(" "));
  const best = candidates
    .filter((word) => !STOPWORDS.has(word) && /^[\p{L}\p{N}]+$/u.test(word))
    // Longest first; on a tie, the word typed first.
    .reduce<string | undefined>((best, word) => (!best || word.length > best.length ? word : best), undefined);
  if (!best) return null;
  const root = stem(best);
  let prefix = 0;
  while (prefix < root.length && prefix < best.length && root[prefix] === best[prefix]) prefix++;
  const word = best.slice(0, Math.max(prefix, Math.min(best.length, 3)));
  return word.length >= 2 ? word : null;
}

/** The qualifier values of one key: what a dropdown shows for it. */
export type QualifierState = { kind: "all" } | { kind: "one"; value: string } | { kind: "many" };

export function qualifierState(parsed: ParsedQuery, key: QualifierKey): QualifierState {
  const positive = parsed.qualifiers.filter((q) => q.key === key && !q.negated);
  const negative = parsed.qualifiers.some((q) => q.key === key && q.negated);
  if (!positive.length && !negative) return { kind: "all" };
  if (positive.length === 1 && positive[0].values.length === 1 && !negative) return { kind: "one", value: positive[0].values[0] };
  return { kind: "many" };
}

function partKey(raw: string): QualifierKey | null {
  return readQualifier(raw)?.key ?? null;
}

/**
 * `text` with every qualifier of `key` removed and, unless `value` is null,
 * one `key:value` appended. Other parts are kept exactly as typed.
 */
export function setQualifier(text: string, key: QualifierKey, value: string | null): string {
  const kept = splitQuery(text).filter((part) => partKey(part.raw) !== key).map((part) => part.raw);
  if (value !== null) kept.push(`${key}:${quoteValue(value)}`);
  return kept.join(" ");
}

/** `text` without the one part that `index` names (a chip's remove button). */
export function removePart(text: string, index: number): string {
  return splitQuery(text).filter((_, i) => i !== index).map((part) => part.raw).join(" ");
}

/** One removable piece of the query, as the chip row draws it. */
export interface QueryChip { index: number; label: string; negated: boolean; kind: "filter" | "phrase" | "word" }

/** The filters and phrases in `text`, each with the part index that removes it. */
export function queryChips(text: string): QueryChip[] {
  const chips: QueryChip[] = [];
  splitQuery(text).slice(0, MAX_QUERY_PARTS).forEach((part, index) => {
    const qualifier = readQualifier(part.raw);
    if (qualifier?.key && qualifier.values.length) {
      chips.push({ index, label: `${qualifier.key}: ${qualifier.values.join(" or ")}`, negated: qualifier.negated, kind: "filter" });
    } else if (/^-?"/.test(part.raw)) {
      const negated = part.raw.startsWith("-");
      chips.push({ index, label: `“${unquote(negated ? part.raw.slice(1) : part.raw)}”`, negated, kind: "phrase" });
    } else if (part.raw.startsWith("-") && part.raw.length > 1) {
      chips.push({ index, label: part.raw.slice(1), negated: true, kind: "word" });
    }
  });
  return chips;
}

/**
 * A saved view's old dropdown filters as query text.
 *
 * Views saved before the query language kept a `facet` object beside the
 * search; applying one now folds it into the text, so it reads the same.
 */
export function facetToQuery(facet: TaskFacet | null | undefined): string {
  const f = facet ?? emptyFacet();
  const parts: string[] = [];
  const priority = normalizePriority(f.priority);
  if (priority !== "all") parts.push(`priority:${PRIORITY_NAMES[priority]}`);
  if (f.kind !== "all") parts.push(`kind:${quoteValue(f.kind)}`);
  if (f.owner === "") parts.push("is:unassigned");
  else if (f.owner !== "all") parts.push(`owner:${quoteValue(f.owner)}`);
  if (f.label !== "all") parts.push(`label:${quoteValue(f.label)}`);
  if (f.due === "overdue") parts.push("is:overdue");
  else if (f.due === "soon") parts.push("is:soon");
  else if (f.due === "none") parts.push("is:no-due");
  return parts.join(" ");
}

/** `search` with a legacy facet folded in, without repeating a part already typed. */
export function mergeLegacyFacet(search: string, facet: TaskFacet | null | undefined): string {
  const typed = new Set(splitQuery(search).map((part) => part.raw));
  const extra = splitQuery(facetToQuery(facet)).map((part) => part.raw).filter((raw) => !typed.has(raw));
  return [search.trim(), ...extra].filter(Boolean).join(" ").slice(0, MAX_QUERY);
}

/** The part the caret is in, for autocomplete: its index and text up to the caret. */
export function partAtCaret(text: string, caret: number): { index: number; part: QueryPart; prefix: string } | null {
  const parts = splitQuery(text);
  const index = parts.findIndex((part) => caret >= part.start && caret <= part.end);
  if (index === -1) return null;
  return { index, part: parts[index], prefix: text.slice(parts[index].start, caret) };
}

/**
 * `text` with the part at `index` replaced by `raw`, and where the caret goes:
 * after a trailing space to keep typing, or straight after a bare `key:`.
 */
export function replacePart(text: string, index: number, raw: string): { text: string; caret: number } {
  const parts = splitQuery(text).map((part) => part.raw);
  if (index < 0 || index >= parts.length) return { text, caret: text.length };
  parts[index] = raw;
  const before = parts.slice(0, index + 1).join(" ");
  const after = parts.slice(index + 1).join(" ");
  const gap = raw.endsWith(":") ? "" : " ";
  const joined = `${before}${gap}${after ? (gap ? "" : " ") + after : ""}`;
  return { text: joined.slice(0, MAX_QUERY), caret: Math.min(before.length + gap.length, MAX_QUERY) };
}

/** An `is:` value as the parser reads it, or null. */
export function isValue(value: string): IsValue | null {
  return IS_ALIASES[fold(value).trim().replace(/[\s_]+/g, "-")] ?? null;
}

/** `is:` states that answer one question and replace each other: due date, or ownership. */
export const IS_GROUPS = {
  due: ["overdue", "soon", "due", "no-due"],
  owner: ["assigned", "unassigned"],
} as const satisfies Record<string, readonly IsValue[]>;

/**
 * `text` with the `is:` parts of one group removed and, unless `value` is
 * null, `is:value` appended. `is:` parts naming other states are kept, so
 * picking a due filter never drops `is:unassigned`.
 */
export function setIsState(text: string, group: readonly IsValue[], value: IsValue | null): string {
  const kept = splitQuery(text).filter((part) => {
    const q = readQualifier(part.raw);
    if (q?.key !== "is" || !q.values.length) return true;
    return !q.values.every((v) => { const state = isValue(v); return state !== null && group.includes(state); });
  }).map((part) => part.raw);
  if (value !== null) kept.push(`is:${value}`);
  return kept.join(" ");
}

/** The one positive `is:` state of a group in the query, "all", or "many". */
export function isState(parsed: ParsedQuery, group: readonly IsValue[]): QualifierState {
  const hits = parsed.qualifiers.filter((q) => q.key === "is" && q.values.some((v) => group.includes(v as IsValue)));
  if (!hits.length) return { kind: "all" };
  if (hits.length === 1 && !hits[0].negated && hits[0].values.length === 1) return { kind: "one", value: hits[0].values[0] };
  return { kind: "many" };
}

/** One autocomplete row: what it shows and the part it puts in place of the one being typed. */
export interface QuerySuggestion { label: string; detail: string; insert: string }

/** The values each qualifier can offer, most useful first. */
export type SuggestionSources = Partial<Record<QualifierKey, readonly { value: string; count?: number }[]>>;

const KEY_HELP: Readonly<Record<QualifierKey, string>> = {
  repo: "in a repository", label: "carrying a label", owner: "owned by", kind: "of a type", status: "in a column",
  priority: "urgent, high, normal, low", severity: "of a severity", is: "overdue, soon, unassigned…",
};
export const MAX_SUGGESTIONS = 8;

/**
 * Completions for the part being typed: qualifier names once two letters
 * match one, then that qualifier's values (after the last comma of a list).
 * Fixed vocabularies (status, priority, is) are offered whatever was loaded.
 */
export function suggest(prefix: string, sources: SuggestionSources): QuerySuggestion[] {
  const match = /^(-?)([A-Za-z_]+):(.*)$/s.exec(prefix);
  if (match) {
    const key = ALIASES[match[2].toLowerCase()];
    if (!key) return [];
    const rest = match[3];
    const comma = rest.lastIndexOf(",");
    const head = rest.startsWith('"') ? "" : rest.slice(0, comma + 1);
    const partial = fold(unquote(rest.startsWith('"') ? rest : rest.slice(comma + 1))).trim();
    const fixed: readonly { value: string; count?: number }[] =
      key === "status" ? STATUSES.map((value) => ({ value }))
        : key === "priority" ? PRIORITY_NAMES.map((value) => ({ value }))
          : key === "is" ? IS_VALUES.map((value) => ({ value }))
            : sources[key] ?? [];
    const starts = fixed.filter(({ value }) => fold(value).startsWith(partial));
    const contains = fixed.filter(({ value }) => !fold(value).startsWith(partial) && fold(value).includes(partial));
    return [...starts, ...contains]
      .filter(({ value }) => fold(value) !== partial || partial === "")
      .slice(0, MAX_SUGGESTIONS)
      .map(({ value, count }) => ({
        label: `${key}:${value}`,
        detail: count === undefined ? "" : plural(count),
        insert: `${match[1]}${match[2]}:${head}${quoteValue(value)}`,
      }));
  }
  const word = fold(prefix.replace(/^-/, ""));
  if (word.length < 2 || !/^[a-z]+$/.test(word)) return [];
  return QUALIFIER_KEYS.filter((key) => key.startsWith(word) && key !== word)
    .map((key) => ({ label: `${key}:`, detail: KEY_HELP[key], insert: `${prefix.startsWith("-") ? "-" : ""}${key}:` }));
}

function plural(count: number): string {
  return `${count} ${count === 1 ? "task" : "tasks"}`;
}
