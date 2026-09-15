/**
 * One-line task entry: `Fix retry loop !1 #reliability @ada ~bug ^GitPulse due:fri :: notes`
 *
 * The board's fastest path to a saved task. Everything here is a pure
 * function of the typed text plus the catalog the reader can actually see, so
 * the preview under the input and the draft that gets saved are computed by
 * the same code — a preview that could disagree with the save would be worse
 * than no preview.
 *
 * ## Why a hand-written scanner
 *
 * Every value is extracted by a single left-to-right pass over words. There
 * is no backtracking regex anywhere in the token path: a quick-add field
 * re-parses on every keystroke, and a pattern with nested quantifiers is how
 * a text box becomes a freeze. The regexes that do appear (`DATE_ABSOLUTE`,
 * `RELATIVE_OFFSET`) are anchored, fixed-length and alternation-free.
 *
 * ## What is and is not a token
 *
 * A marker only counts at the start of a word — `C#` is not a label and
 * `ada@example.com` is not an owner. A marker with an empty body is literal
 * text, so a lone `#` types through. A backslash escapes a leading marker.
 * Only the title segment is scanned; whatever follows `::` (or the first
 * newline) is prose and is copied verbatim, so a description can contain
 * every marker without being eaten.
 *
 * ## Honesty
 *
 * Anything the parser could not honour becomes a `warning`, never a silent
 * drop: an unknown repository name, a priority outside 0-3, a date it cannot
 * read, labels past the cap. The board shows warnings beside the preview so a
 * reader never saves a task that quietly lost half of what they typed.
 */

import type { EnhancementField, TaskDraft, TaskStatus } from "./client";

/** Longest quick-add line accepted. Past this the input is refused, not truncated. */
export const MAX_QUICK_ADD_LENGTH = 4_096;
/** Labels one line may set. */
export const MAX_QUICK_ADD_LABELS = 12;
/** Title cap, matching the workbench's own 300-character title limit. */
export const MAX_QUICK_ADD_TITLE = 300;
/** Hour of day a bare date means, so "due:friday" is end of Friday. */
export const QUICK_ADD_DUE_HOUR = 17;

export type QuickAddTokenKind =
  | "text"
  | "priority"
  | "label"
  | "owner"
  | "kind"
  | "repository"
  | "due"
  | "separator"
  | "description"
  | "unknown";

export interface QuickAddSegment {
  text: string;
  kind: QuickAddTokenKind;
}

export interface QuickAddWarning {
  code:
    | "too_long"
    | "unknown_repository"
    | "bad_priority"
    | "bad_due"
    | "label_cap"
    | "empty_title";
  message: string;
}

export interface QuickAddResult {
  title: string;
  description: string;
  priority: number | null;
  labels: string[];
  owner: string | null;
  kind: string | null;
  /** Repository the `^` token resolved to, by id. */
  repositoryId: string | null;
  /** Epoch seconds, local wall clock, or null. */
  dueAt: number | null;
  /** Coloured runs covering the whole input, in order, for the preview. */
  segments: QuickAddSegment[];
  warnings: QuickAddWarning[];
  /** True when this line could be saved as a task right now. */
  usable: boolean;
}

export interface QuickAddRepository {
  id: string;
  name: string;
}

export interface QuickAddOptions {
  repositories?: readonly QuickAddRepository[];
  /** Epoch milliseconds; injected so date tokens are testable. */
  now?: number;
}

/**
 * Every lookup table here has a null prototype, and every read goes through
 * `Object.hasOwn`.
 *
 * Both, not either: `PRIORITY_WORDS["constructor"]` on a plain object literal
 * returns `Object.prototype.constructor`, so `!constructor` set `priority` to
 * a function — which then flowed into a task draft and out over IPC. The null
 * prototype removes the inherited keys; `hasOwn` keeps the intent obvious to
 * the next reader who adds a table.
 */
const MARKERS: Record<string, QuickAddTokenKind> = Object.assign(Object.create(null), {
  "!": "priority",
  "#": "label",
  "@": "owner",
  "~": "kind",
  "^": "repository",
});

const PRIORITY_WORDS: Record<string, number> = Object.assign(Object.create(null), {
  "0": 0, urgent: 0, p0: 0,
  "1": 1, high: 1, p1: 1,
  "2": 2, normal: 2, medium: 2, p2: 2,
  "3": 3, low: 3, p3: 3,
});

const WEEKDAYS: Record<string, number> = Object.assign(Object.create(null), {
  sun: 0, sunday: 0,
  mon: 1, monday: 1,
  tue: 2, tues: 2, tuesday: 2,
  wed: 3, weds: 3, wednesday: 3,
  thu: 4, thur: 4, thurs: 4, thursday: 4,
  fri: 5, friday: 5,
  sat: 6, saturday: 6,
});

/**
 * `2026-09-12`, `2026-09-12T14:30`, `2026-09-12 14:30`.
 *
 * The separator class carries `t` and `-` because the caller lowercases and
 * collapses whitespace to `-` first (so `due:"next week"` can become
 * `next-week`). With only `[T ]` here, every `due:…T14:30` silently returned
 * null and warned that a perfectly good date was unreadable. Anchored,
 * fixed-width and alternation-free, so there is nothing to backtrack.
 */
const DATE_ABSOLUTE = /^(\d{4})-(\d{2})-(\d{2})(?:[t\- ](\d{2}):(\d{2}))?$/;
/** `+3d`, `+2w`, `+1m`. Bounded digits so the quantifier cannot run away. */
const RELATIVE_OFFSET = /^\+(\d{1,4})([dwm])$/;

/** Control characters strip out; the rest of the text is left alone. */
function clean(value: string): string {
  let out = "";
  for (const ch of value) {
    const code = ch.codePointAt(0) ?? 0;
    if (code === 9 || code === 10 || (code >= 32 && code !== 127)) out += ch;
  }
  return out;
}

interface Word {
  text: string;
  start: number;
  end: number;
}

/**
 * Split on whitespace, keeping a double-quoted run together.
 *
 * Quoting is what lets `@"Ada Lovelace"` and `#"needs design"` exist without
 * a second grammar. An unterminated quote simply runs to end of input rather
 * than invalidating the line — a reader mid-type has an unterminated quote on
 * almost every keystroke.
 */
function splitWords(text: string): Word[] {
  const words: Word[] = [];
  let index = 0;
  while (index < text.length) {
    while (index < text.length && /\s/.test(text[index])) index += 1;
    if (index >= text.length) break;
    const start = index;
    let quoted = false;
    while (index < text.length) {
      const ch = text[index];
      if (ch === '"') quoted = !quoted;
      else if (!quoted && /\s/.test(ch)) break;
      index += 1;
    }
    words.push({ text: text.slice(start, index), start, end: index });
  }
  return words;
}

function unquote(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')) {
    return trimmed.slice(1, -1).trim();
  }
  return trimmed.replace(/"/g, "").trim();
}

function startOfDay(date: Date): Date {
  const copy = new Date(date.getTime());
  copy.setHours(0, 0, 0, 0);
  return copy;
}

function atDueHour(date: Date): Date {
  const copy = startOfDay(date);
  copy.setHours(QUICK_ADD_DUE_HOUR, 0, 0, 0);
  return copy;
}

/**
 * A due-date word to epoch seconds, or null when it is not a date.
 *
 * Deliberately does not fall back to `Date.parse`: that accepts
 * implementation-defined strings, so `due:next` could become a date on one
 * engine and nothing on another. Anything not listed here is reported as a
 * warning instead of guessed at.
 */
export function parseQuickAddDue(value: unknown, now = Date.now()): number | null {
  if (typeof value !== "string") return null;
  const text = value.trim().toLowerCase().replace(/\s+/g, "-");
  if (!text || text.length > 32) return null;
  const base = Number.isFinite(now) ? new Date(now) : new Date();
  if (!Number.isFinite(base.getTime())) return null;

  if (text === "today" || text === "tod" || text === "eod") return seconds(atDueHour(base));
  if (text === "tomorrow" || text === "tom") return seconds(atDueHour(shiftDays(base, 1)));
  if (text === "next-week") return seconds(atDueHour(shiftDays(base, 7)));

  const offset = RELATIVE_OFFSET.exec(text);
  if (offset) {
    const amount = Number(offset[1]);
    if (!Number.isSafeInteger(amount)) return null;
    const unit = offset[2];
    const days = unit === "d" ? amount : unit === "w" ? amount * 7 : 0;
    const shifted = unit === "m" ? shiftMonths(base, amount) : shiftDays(base, days);
    return seconds(atDueHour(shifted));
  }

  const weekdayName = text.startsWith("next-") ? text.slice(5) : text;
  if (Object.hasOwn(WEEKDAYS, weekdayName)) {
    const target = WEEKDAYS[weekdayName];
    const delta = (target - base.getDay() + 7) % 7;
    // A bare weekday always means the *next* one; "today" needs the word.
    return seconds(atDueHour(shiftDays(base, delta === 0 ? 7 : delta)));
  }

  const absolute = DATE_ABSOLUTE.exec(text);
  if (absolute) {
    const [, year, month, day, hour, minute] = absolute;
    const y = Number(year);
    const m = Number(month);
    const d = Number(day);
    const h = hour === undefined ? QUICK_ADD_DUE_HOUR : Number(hour);
    const min = minute === undefined ? 0 : Number(minute);
    if (m < 1 || m > 12 || d < 1 || d > 31 || h > 23 || min > 59) return null;
    const date = new Date(y, m - 1, d, h, min, 0, 0);
    if (!Number.isFinite(date.getTime())) return null;
    // Reject 2026-02-31: the Date constructor would roll it into March.
    if (date.getFullYear() !== y || date.getMonth() !== m - 1 || date.getDate() !== d) return null;
    return seconds(date);
  }
  return null;
}

function shiftDays(date: Date, days: number): Date {
  const copy = new Date(date.getTime());
  copy.setDate(copy.getDate() + days);
  return copy;
}

function shiftMonths(date: Date, months: number): Date {
  const copy = new Date(date.getTime());
  copy.setMonth(copy.getMonth() + months);
  return copy;
}

function seconds(date: Date): number | null {
  const value = Math.floor(date.getTime() / 1000);
  return Number.isSafeInteger(value) && value > 0 ? value : null;
}

/**
 * Resolve `^name` against the repositories this board actually lists.
 *
 * Exact case-insensitive name first, then a unique case-insensitive prefix.
 * An ambiguous prefix resolves to nothing and warns, because picking one of
 * two repositories for the reader is how a task lands in the wrong project.
 */
export function matchQuickAddRepository(
  query: string,
  repositories: readonly QuickAddRepository[],
): QuickAddRepository | null {
  const needle = query.trim().toLowerCase();
  if (!needle) return null;
  const exact = repositories.filter((repo) => repo.name.toLowerCase() === needle);
  if (exact.length === 1) return exact[0];
  if (exact.length > 1) return null;
  const prefixed = repositories.filter((repo) => repo.name.toLowerCase().startsWith(needle));
  return prefixed.length === 1 ? prefixed[0] : null;
}

/** Split the tokenized head from the verbatim description tail. */
function splitBody(text: string): { head: string; tail: string; separator: string } {
  const marker = text.indexOf("::");
  const newline = text.indexOf("\n");
  if (marker >= 0 && (newline < 0 || marker < newline)) {
    return { head: text.slice(0, marker), tail: text.slice(marker + 2), separator: "::" };
  }
  if (newline >= 0) return { head: text.slice(0, newline), tail: text.slice(newline + 1), separator: "\n" };
  return { head: text, tail: "", separator: "" };
}

export function parseQuickAdd(input: unknown, options: QuickAddOptions = {}): QuickAddResult {
  const repositories = options.repositories ?? [];
  const now = options.now ?? Date.now();
  const warnings: QuickAddWarning[] = [];
  const raw = typeof input === "string" ? input : "";

  if (raw.length > MAX_QUICK_ADD_LENGTH) {
    return {
      title: "", description: "", priority: null, labels: [], owner: null, kind: null,
      repositoryId: null, dueAt: null,
      segments: [{ text: raw.slice(0, 200), kind: "text" }],
      warnings: [{
        code: "too_long",
        message: `This line is ${raw.length} characters. Keep quick add under ${MAX_QUICK_ADD_LENGTH}, or open the full editor.`,
      }],
      usable: false,
    };
  }

  const text = clean(raw);
  const { head, tail, separator } = splitBody(text);
  const segments: QuickAddSegment[] = [];
  const titleParts: string[] = [];
  const labels: string[] = [];
  let priority: number | null = null;
  let owner: string | null = null;
  let kind: string | null = null;
  let repositoryId: string | null = null;
  let dueAt: number | null = null;
  let labelCapHit = false;

  // One pass: every word either sets a field or becomes part of the title,
  // and either way contributes exactly one coloured segment. The segments
  // concatenate back to the input, so the preview can never show text the
  // parser did not actually look at.
  let cursor = 0;
  for (const word of splitWords(head)) {
    if (word.start > cursor) segments.push({ text: head.slice(cursor, word.start), kind: "text" });
    cursor = word.end;
    segments.push({ text: word.text, kind: consume(word.text) });
  }
  if (cursor < head.length) segments.push({ text: head.slice(cursor), kind: "text" });

  function consume(word: string): QuickAddTokenKind {
    if (word.startsWith("\\") && word.length > 1 && word[1] in MARKERS) {
      titleParts.push(word.slice(1));
      return "text";
    }
    const lower = word.toLowerCase();
    for (const prefix of ["due:", "by:"]) {
      if (lower.startsWith(prefix) && word.length > prefix.length) {
        const value = unquote(word.slice(prefix.length));
        const parsed = parseQuickAddDue(value, now);
        if (parsed === null) {
          warnings.push({
            code: "bad_due",
            message: `"${value}" is not a date I can read. Try today, tomorrow, friday, +3d, or 2026-09-30.`,
          });
          return "unknown";
        }
        dueAt = parsed;
        return "due";
      }
    }
    const marker = word[0];
    const markerKind = Object.hasOwn(MARKERS, marker) ? MARKERS[marker] : undefined;
    if (!markerKind) {
      titleParts.push(word);
      return "text";
    }
    const body = unquote(word.slice(1));
    if (!body) {
      titleParts.push(word);
      return "text";
    }
    switch (markerKind) {
      case "priority": {
        const word = body.toLowerCase();
        const value = Object.hasOwn(PRIORITY_WORDS, word) ? PRIORITY_WORDS[word] : undefined;
        if (value === undefined) {
          warnings.push({
            code: "bad_priority",
            message: `"!${body}" is not a priority. Use !0-!3 or !urgent, !high, !normal, !low.`,
          });
          return "unknown";
        }
        priority = value;
        return "priority";
      }
      case "label": {
        if (labels.includes(body)) return "label";
        if (labels.length >= MAX_QUICK_ADD_LABELS) {
          labelCapHit = true;
          return "unknown";
        }
        labels.push(body);
        return "label";
      }
      case "owner":
        owner = body;
        return "owner";
      case "kind":
        kind = body.toLowerCase();
        return "kind";
      case "repository": {
        const match = matchQuickAddRepository(body, repositories);
        if (!match) {
          warnings.push({
            code: "unknown_repository",
            message: repositories.length
              ? `No repository here matches "^${body}". It stays in the title.`
              : `"^${body}" needs a registered repository. It stays in the title.`,
          });
          titleParts.push(word);
          return "unknown";
        }
        repositoryId = match.id;
        return "repository";
      }
      default:
        titleParts.push(word);
        return "text";
    }
  }

  if (separator) segments.push({ text: separator, kind: "separator" });
  if (tail) segments.push({ text: tail, kind: "description" });

  if (labelCapHit) {
    warnings.push({
      code: "label_cap",
      message: `Quick add carries at most ${MAX_QUICK_ADD_LABELS} labels. Add the rest in the editor.`,
    });
  }

  let title = titleParts.join(" ").replace(/\s+/g, " ").trim();
  if (title.length > MAX_QUICK_ADD_TITLE) title = `${title.slice(0, MAX_QUICK_ADD_TITLE - 1)}…`;

  // No separate description cap: `MAX_QUICK_ADD_LENGTH` already bounds the
  // whole line, so a second, larger limit here would be unreachable code
  // claiming a guarantee it never enforces.
  const description = tail.trim();

  if (!title && text.trim()) {
    warnings.push({ code: "empty_title", message: "Add a few words of title before the markers." });
  }

  return {
    title, description, priority, labels, owner, kind, repositoryId, dueAt,
    segments: segments.filter((segment) => segment.text.length > 0),
    warnings,
    usable: title.length > 0,
  };
}

/**
 * Turn a parsed line into the draft the editor and the board both save.
 *
 * Unset tokens fall through to the caller's defaults rather than to zeroes:
 * a line with no `!` must not silently become Urgent, and a line with no `^`
 * must keep the column's own repository.
 */
export function quickAddDraft(
  parsed: QuickAddResult,
  defaults: {
    status: TaskStatus;
    kind: string;
    repositoryIds: readonly string[];
    primaryRepositoryId: string;
    homeWorkspaceId: string | null;
    position: number;
  },
): TaskDraft | null {
  if (!parsed.usable) return null;
  const repositoryIds = parsed.repositoryId
    ? [parsed.repositoryId, ...defaults.repositoryIds.filter((id) => id !== parsed.repositoryId)]
    : [...defaults.repositoryIds];
  if (!repositoryIds.length) return null;
  const primary = parsed.repositoryId
    ?? (repositoryIds.includes(defaults.primaryRepositoryId) ? defaults.primaryRepositoryId : repositoryIds[0]);
  return {
    title: parsed.title,
    description: parsed.description,
    kind: parsed.kind || defaults.kind,
    status: defaults.status,
    priority: parsed.priority ?? 2,
    severity: null,
    owner: parsed.owner,
    due_at: parsed.dueAt,
    labels: [...parsed.labels],
    acceptance_criteria: [],
    repository_ids: repositoryIds,
    primary_repository_id: primary,
    home_workspace_id: defaults.homeWorkspaceId,
    position: defaults.position,
    locked_fields: [],
  };
}

/** The one-line syntax reminder shown under the quick-add field. */
export const QUICK_ADD_HINT = "!priority  #label  @owner  ~type  ^repo  due:friday  ::notes";

/** Which Return the reader gets: save the line, or save it and ask for a draft. */
export type QuickAddMode = "manual" | "assist";

export function isQuickAddMode(value: unknown): value is QuickAddMode {
  return value === "manual" || value === "assist";
}

export interface QuickAddAssistPlan {
  /** Fields the model would be asked for; empty when the line is not a task. */
  asks: EnhancementField[];
  /** One sentence stating what happens on Return, before anything is written. */
  sentence: string;
}

const ASK_NAMES: Readonly<Record<EnhancementField, string>> = Object.freeze(
  Object.assign(Object.create(null) as Record<EnhancementField, string>, {
    title: "a title",
    description: "a description",
  }),
);

/**
 * What the drafting mode would do with this line, worked out before it runs.
 *
 * Two rules, and neither is a tiebreak applied afterwards:
 *
 * 1. **Markers always win, structurally.** The task is written from `parseQuickAdd`
 *    first; the model is asked afterwards and proposes against the saved
 *    result. It cannot overwrite anything — acceptance is a separate, explicit
 *    step in the review.
 * 2. **The model cannot touch the marker fields at all.** `EnhancementField` is
 *    `"title" | "description"`, enforced by the store's decoder and the field
 *    lock table, so priority, labels, owner, type, repository and due date are
 *    out of scope by type rather than by policy.
 *
 * The sentence lives here, beside `asks` and derived from it, rather than in
 * the component: the promise shown before Return and the request made after it
 * then come from one value, and a field cannot be named on screen that the
 * request does not carry.
 */
export function quickAddAssistPlan(parsed: QuickAddResult): QuickAddAssistPlan {
  // Gated on the same `usable` the manual path uses. Drafting improves a task;
  // it cannot invent one, and this is what stops a placeholder card ever
  // reaching the board while a model thinks.
  if (!parsed.usable) {
    return { asks: [], sentence: "Type a title first. Drafting improves a task; it does not invent one." };
  }
  // Both fields, always. A description the reader did not write is the point of
  // the mode, and a title they did write is still worth a proposal — they keep
  // theirs unless they accept the replacement.
  const asks: EnhancementField[] = ["title", "description"];
  // Built from `asks` rather than written out, so the promise on screen cannot
  // name a field the request does not carry.
  const names = asks.map((field) => ASK_NAMES[field]).join(" and ");
  return {
    asks,
    sentence: `Saves this line now, then asks for ${names} you can accept or reject. Nothing else changes.`,
  };
}
