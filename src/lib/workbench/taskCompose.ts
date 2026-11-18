import { priorityLabel } from "./taskPriority";
import { STATUS_LABELS, type TaskDraft, type TaskStatus } from "./client";
import { displayTitle } from "./taskDelete";
import { formatLogsSection } from "./taskLogs";
import { LIST_BULLET as BULLET, SUBTASK_PLACEHOLDER as PLACEHOLDER_CRITERIA, stripListMarker } from "./intake";
import AGENT_GUIDANCE from "../../../src-tauri/vendored/dc-store/src/workbench/agent_guidance.md?raw";

export const MAX_AGENT_COPY_TASKS = 8;
export const MAX_SUBTASKS = 128;
export const SUBTASK_CAP = 4096;

const CHECKLIST_NAMES = String.raw`(?:task\s+)?(?:sub[- ]?tasks?|checklist|acceptance\s+criteria|ac|requirements|definition\s+of\s+done|done\s+when|tasks|to-?\s?dos?)`;
/** A checklist's name, once heading marks, emphasis, a trailing note in brackets and the colon are off. */
const CHECKLIST_NAME = new RegExp(String.raw`^${CHECKLIST_NAMES}$`, "i");
const BULLET_OR_NUMBER_LINE = new RegExp(String.raw`^\s*${BULLET}\s+(.+)$`);
const CHECKBOX_LINE = new RegExp(String.raw`^\s*(?:${BULLET}\s*)?\[([ xX])\]\s*(.+)$`);
const BOX_GLYPH_LINE = /^\s*(?:[-*+•]\s*)?[☐☑☒]\s*(.+)$/;
/** A Markdown code fence: a checklist-looking line inside one is code, not a task. */
const FENCE = /^\s{0,3}(`{3,}|~{3,})/;
const ATX_HEADING = /^#{1,6}\s+/;
const HEADING_MAX = 200;

/** A Markdown list line: a checkbox, a bullet, or a numbered item. */
function isListText(text: string): boolean {
  return checkboxText(text) !== null || BULLET_OR_NUMBER_LINE.test(text);
}

/** The item text of a checkbox line, in any of the shapes it is pasted in. */
function checkboxText(line: string): string | null {
  return line.match(CHECKBOX_LINE)?.[2] ?? line.match(BOX_GLYPH_LINE)?.[1] ?? null;
}

/** A line's text without heading marks, wrapping emphasis, a trailing "(…)" note and a final colon. */
function headingName(line: string): { name: string; colon: boolean; heading: boolean } {
  let text = line.trim();
  // A heading is a short line. Long ones are prose, and skipping them keeps the
  // bracket-note pattern below off a 64 KB line on every keystroke.
  if (text.length > HEADING_MAX) return { name: text, colon: false, heading: false };
  const heading = ATX_HEADING.test(text) || /^(\*\*|__).+\1:?$/.test(text);
  text = text.replace(ATX_HEADING, "").trim();
  for (let i = 0; i < 2; i++) text = text.replace(/^(\*\*|__)(.*)\1(:?)$/, "$2$3").trim();
  const colon = text.endsWith(":");
  text = text.replace(/:$/, "").replace(/\s*\([^)]*\)$/, "").trim();
  return { name: text, colon, heading };
}

/**
 * What a line does to the list around it: opens a checklist, ends one (any
 * other heading), or neither. A bare name opens a list only as a heading or
 * when it ends in a colon, so a sentence that happens to start "Tasks" does not.
 */
function headingKind(line: string): "checklist" | "other" | null {
  const { name, colon, heading } = headingName(line);
  if ((heading || colon) && CHECKLIST_NAME.test(name)) return "checklist";
  return ATX_HEADING.test(line.trim()) ? "other" : null;
}

/** At most `max` UTF-8 bytes, never splitting a character: the store's own unit. */
function capBytes(text: string, max: number): string {
  const encoder = new TextEncoder();
  if (encoder.encode(text).length <= max) return text;
  let used = 0;
  let out = "";
  for (const ch of text) {
    const size = encoder.encode(ch).length;
    if (used + size > max) break;
    used += size;
    out += ch;
  }
  return out;
}

function cleanSubtaskItem(raw: string, capped = true): string {
  const text = stripListMarker(raw);
  if (PLACEHOLDER_CRITERIA.test(text)) return "";
  // The store bounds a criterion in bytes; a character cap let a long CJK line
  // through here and fail at save with nothing naming the row.
  return capped ? capBytes(text, SUBTASK_CAP) : text;
}

export function mergeSubtasks(
  existing: readonly string[],
  incoming: readonly string[],
  cap = MAX_SUBTASKS,
): string[] {
  const limit = Number.isSafeInteger(cap) && cap > 0 ? Math.min(cap, MAX_SUBTASKS) : MAX_SUBTASKS;
  const result: string[] = [];
  const seen = new Set<string>();

  for (const item of existing ?? []) {
    const cleaned = cleanSubtaskItem(typeof item === "string" ? item : "");
    if (!cleaned) continue;
    const key = cleaned.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      result.push(cleaned);
    }
  }

  for (const item of incoming ?? []) {
    if (result.length >= limit) break;
    const cleaned = cleanSubtaskItem(typeof item === "string" ? item : "");
    if (!cleaned) continue;
    const key = cleaned.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      result.push(cleaned);
    }
  }

  return result.slice(0, limit);
}

/** What pulling a checklist out of some text found, and what it left. */
export interface ChecklistExtraction {
  /** Criteria the text held that `existing` did not, in order and without repeats. */
  subtasks: string[];
  /** The text without the lines that were taken. Unchanged when nothing was. */
  remainingText: string;
  /** Items taken out because the same criterion was already listed. */
  duplicates: number;
  /** Items left in the text because the criteria list had no room for them. */
  overflow: number;
}

function leadingWidth(line: string): number {
  return line.length - line.trimStart().length;
}

/**
 * Whether `next` is the same sentence as `line`, hard-wrapped: "…when the" /
 * "network drops". A lowercase start or a line ending mid-clause carries on.
 */
function carriesOn(line: string, next: string): boolean {
  return /^\p{Ll}/u.test(next) || /[,;:(\-–—]$/.test(line.trim());
}

/**
 * Lines with the removed ones (`null`) taken out.
 *
 * Blank lines are collapsed only where a removal made them meet, so a
 * description whose author spaced it on purpose (a code block, a poem, a
 * table) keeps its spacing everywhere the extraction did not touch it.
 */
function joinKept(out: readonly (string | null)[]): string {
  const result: string[] = [];
  for (let i = 0; i < out.length;) {
    const entry = out[i];
    if (entry !== null && entry.trim() !== "") { result.push(entry); i++; continue; }
    let j = i;
    let removed = false;
    let blank = false;
    while (j < out.length && (out[j] === null || out[j]!.trim() === "")) {
      if (out[j] === null) removed = true; else blank = true;
      j++;
    }
    if (!removed) for (let k = i; k < j; k++) result.push(out[k]!);
    // A list that lost an item from its middle stays one list; anything else
    // that a removal brought together keeps a paragraph break between.
    else if (result.length > 0 && j < out.length && (blank || !isListText(out[j]!.trim()))) result.push("");
    i = j;
  }
  while (result.length && result[0]!.trim() === "") result.shift();
  while (result.length && result.at(-1)!.trim() === "") result.pop();
  return result.join("\n");
}

/** What appending to a criteria list did. */
export interface CriteriaAppend {
  /** The rows as they were (trimmed, blanks dropped), then the new ones. */
  list: string[];
  added: string[];
  /** Offered items already in the list. */
  duplicates: number;
  /** Offered items past the cap. */
  overflow: number;
  /** Added items cut to the store's 4 KB per criterion. */
  truncated: number;
}

/**
 * Add criteria after the rows a person already has, without touching them.
 *
 * `mergeSubtasks` re-cleans and dedupes its first argument too, which is right
 * for building a list from scratch and wrong for appending to one being edited:
 * adding "New" used to drop one of two rows that differed only in case, or a
 * row reading "N/A", and then report "1 not added". Incoming items are cleaned
 * (markers off, placeholders dropped, bytes capped); existing rows are not.
 */
export function appendCriteria(rows: readonly string[], incoming: readonly string[], cap = MAX_SUBTASKS): CriteriaAppend {
  const limit = Number.isSafeInteger(cap) && cap > 0 ? Math.min(cap, MAX_SUBTASKS) : MAX_SUBTASKS;
  const list = (rows ?? []).map((row) => (typeof row === "string" ? row.trim() : "")).filter(Boolean);
  const seen = new Set(list.map((row) => (cleanSubtaskItem(row) || row).toLowerCase()));
  const added: string[] = [];
  let duplicates = 0;
  let overflow = 0;
  let truncated = 0;
  for (const raw of incoming ?? []) {
    const text = typeof raw === "string" ? raw : "";
    const cleaned = cleanSubtaskItem(text);
    if (!cleaned) continue;
    const key = cleaned.toLowerCase();
    if (seen.has(key)) { duplicates++; continue; }
    if (list.length + added.length >= limit) { overflow++; continue; }
    if (cleaned !== cleanSubtaskItem(text, false)) truncated++;
    seen.add(key);
    added.push(cleaned);
  }
  return { list: [...list, ...added], added, duplicates, overflow, truncated };
}

/** The sentence for what an append refused, or "" when it took everything. */
export function describeRefusedCriteria(result: Pick<CriteriaAppend, "duplicates" | "overflow"> & { truncated?: number }): string {
  const parts: string[] = [];
  if (result.duplicates > 0) parts.push(`${result.duplicates} not added: already listed.`);
  if (result.overflow > 0) parts.push(`${result.overflow} not added: the list holds ${MAX_SUBTASKS}.`);
  if ((result.truncated ?? 0) > 0) parts.push(`${result.truncated} shortened to 4 KB, the most a criterion holds.`);
  return parts.join(" ");
}

/**
 * A pasted block, as candidate criteria in the order it was pasted.
 *
 * Only a line naming a checklist ("Subtasks:", "## Acceptance criteria") is
 * dropped — it labels the list rather than being an item. Every other line is
 * offered, markers and all; `appendCriteria` strips the markers. Reading the
 * block through the extractor instead moved the checkbox lines ahead of the
 * plain ones and lost a plain list under an unbulleted heading.
 */
export function criteriaFromPaste(text: unknown): string[] {
  if (typeof text !== "string") return [];
  return text.split(/\r?\n/).filter((line) => line.trim() && headingKind(line.trim()) !== "checklist");
}

/**
 * Pull checklist items out of free text.
 *
 * A `- [ ]` line anywhere is an item; a plain bullet or numbered line is one
 * only under a heading or `Label:` line that names a checklist (Subtasks,
 * Checklist, Acceptance criteria, Definition of done, To-do), which may have
 * one introducing "Do these in order:" line before its items. Code fences are
 * never read, so a snippet that happens to hold `- [ ]` stays code; an item
 * with a code block under it stays in the text with its code. An item that
 * wraps onto the next line — indented, or hard-wrapped mid-sentence — keeps
 * its whole sentence.
 *
 * Nothing is lost: an item already in `existing` is removed (it is kept
 * there), and an item that does not fit under `cap` stays in the text.
 */
export function extractSubtasksFromContent(
  content: unknown,
  options: { existing?: readonly string[]; cap?: number } = {},
): ChecklistExtraction {
  const empty = { subtasks: [], duplicates: 0, overflow: 0 };
  if (typeof content !== "string" || !content.trim()) {
    return { ...empty, remainingText: typeof content === "string" ? content : "" };
  }
  const limit = Number.isSafeInteger(options.cap) && options.cap! > 0 ? Math.min(options.cap!, MAX_SUBTASKS) : MAX_SUBTASKS;
  const seen = new Set(mergeSubtasks(options.existing ?? [], []).map((item) => item.toLowerCase()));
  // Room is counted from the rows as they stand, the way `appendCriteria`
  // counts it: counting the deduped set took an item out of the text that the
  // append then had no room for, so it was in neither place.
  const rows = (options.existing ?? []).filter((row) => typeof row === "string" && row.trim()).length;
  const room = Math.max(0, limit - rows);

  const lines = content.split(/\r?\n/);
  const out: (string | null)[] = [];
  const subtasks: string[] = [];
  let duplicates = 0;
  let overflow = 0;
  let removedAny = false;
  let fence: { char: string; length: number; nested: boolean } | null = null;
  let section: { heading: number; intro: number | null; removed: number; kept: number } | null = null;
  let item: { text: string; raw: string[]; indent: number } | null = null;

  const closeItem = () => {
    if (!item) return;
    const cleaned = cleanSubtaskItem(item.text);
    const key = cleaned.toLowerCase();
    let take = true;
    if (cleaned && seen.has(key)) duplicates++;
    else if (cleaned && subtasks.length >= room) { overflow++; take = false; }
    else if (cleaned) { seen.add(key); subtasks.push(cleaned); }
    // A placeholder such as "- [ ] None" is taken out and adds nothing.
    if (take) { for (let n = 0; n < item.raw.length; n++) out.push(null); removedAny = true; }
    else out.push(...item.raw);
    if (section) { if (take) section.removed++; else section.kept++; }
    item = null;
  };
  const closeSection = () => {
    // The heading, and the line introducing the list, go only with the whole list.
    if (section && section.removed > 0 && section.kept === 0) {
      out[section.heading] = null;
      if (section.intro !== null) out[section.intro] = null;
    }
    section = null;
  };
  const isListLine = (text: string) => checkboxText(text) !== null || BULLET_OR_NUMBER_LINE.test(text);
  // Whether `text` can be the one line introducing an open section's items:
  // the section has no item yet and no intro, and the line is a "…:" lead-in.
  // Nothing but blank lines can sit between a heading and its first item
  // without closing the section, so "no item yet" also means "right under it".
  const opensIntro = (text: string) => section !== null && section.intro === null && section.removed + section.kept === 0
    && text.endsWith(":") && !isListLine(text) && headingKind(text) === null;
  // The next non-blank line after each index, computed once from the end, so a
  // run of blank lines costs one pass rather than one scan per blank.
  const nextFilled: (string | undefined)[] = new Array(lines.length);
  for (let k = lines.length - 1, after: string | undefined; k >= 0; k--) {
    nextFilled[k] = after;
    if (lines[k]!.trim()) after = lines[k]!.trim();
  }

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const trimmed = line.trim();
    if (fence) {
      // A fence opened inside an item may sit at any depth, and closes at it.
      const close = fence.nested ? trimmed.match(/^(`{3,}|~{3,})/) : line.match(FENCE);
      if (close && close[1]![0] === fence.char && close[1]!.length >= fence.length) fence = null;
      out.push(line);
      continue;
    }
    if (item && leadingWidth(line) > item.indent) {
      const nested = trimmed.match(/^(`{3,}|~{3,})/);
      if (nested) {
        // Code under an item belongs to it. Taking the item and leaving its code
        // under someone else's line loses what the code was for, so the two stay
        // in the text together and the list carries on after them.
        out.push(...item.raw);
        if (section) section.kept++;
        item = null;
        fence = { char: nested[1]![0]!, length: nested[1]!.length, nested: true };
        out.push(line);
        continue;
      }
    }
    const fenceMatch = line.match(FENCE);
    if (fenceMatch) {
      closeItem(); closeSection();
      fence = { char: fenceMatch[1]![0]!, length: fenceMatch[1]!.length, nested: false };
      out.push(line);
      continue;
    }
    if (!trimmed) {
      closeItem();
      // A blank line between two items of one list belongs to the list. One
      // before the line introducing the items ("## Tasks", blank, "Do these
      // in order:") is kept open too: that is how Markdown spaces it.
      const next = nextFilled[i];
      if (section && next && isListLine(next)) out.push(null);
      else if (section && next && opensIntro(next)) out.push(line);
      else { closeSection(); out.push(line); }
      continue;
    }
    const checkbox = checkboxText(trimmed);
    const bullet = section ? trimmed.match(BULLET_OR_NUMBER_LINE) : null;
    const kind = headingKind(trimmed);
    if (item && leadingWidth(line) > item.indent && kind === null) {
      // Indented under an open item: its wrapped text, or — outside a list
      // section, where a plain bullet is not an item of its own — a detail
      // line that belongs to it. Inside a section a nested bullet is a sibling.
      const plain = !isListLine(trimmed);
      const detail = !section && checkbox === null ? trimmed.match(BULLET_OR_NUMBER_LINE) : null;
      if (plain || detail) {
        item.text += plain ? ` ${trimmed}` : `; ${detail![1]!.trim()}`;
        item.raw.push(line);
        continue;
      }
    }
    const tail = item?.raw.at(-1)!.trim() ?? "";
    if (item && kind === null && !isListLine(trimmed) && !/[.!?]$/.test(tail) && carriesOn(tail, trimmed)) {
      // Hard-wrapped at the margin, as mail and terminals paste a long item.
      // An item that ended its sentence takes nothing more: merging the next
      // line would turn prose into part of a criterion.
      item.text += ` ${trimmed}`;
      item.raw.push(line);
      continue;
    }
    closeItem();
    if (kind !== null) {
      closeSection();
      if (kind === "checklist") section = { heading: out.length, intro: null, removed: 0, kept: 0 };
      out.push(line);
      continue;
    }
    if (checkbox !== null) { item = { text: checkbox, raw: [line], indent: leadingWidth(line) }; continue; }
    if (bullet) { item = { text: bullet[1]!, raw: [line], indent: leadingWidth(line) }; continue; }
    if (section && opensIntro(trimmed)) {
      // "## Tasks" / "Do these in order:" / "- a": one line under the heading,
      // before any item, that introduces the items and goes with them.
      section.intro = out.length;
      out.push(line);
      continue;
    }
    closeSection();
    out.push(line);
  }
  closeItem();
  closeSection();

  if (!removedAny) return { ...empty, overflow, remainingText: content };
  let remainingText = joinKept(out);
  if (content.endsWith("\n") && remainingText) remainingText += "\n";
  return { subtasks, remainingText, duplicates, overflow };
}

export function extractSubtasks(text: unknown): string[] {
  return extractSubtasksFromContent(text).subtasks;
}

export interface DraftChecklistExtraction {
  description: string;
  /** The whole criteria list after the move: what was there, then what was found. */
  subtasks: string[];
  /** Whether any line moved. A list that was already complete still moves its lines. */
  extracted: boolean;
  /** Criteria that are new to the list. */
  count: number;
  duplicates: number;
  overflow: number;
}

/**
 * Move the description's checklist into the criteria (the sheet's Extract).
 *
 * Notes are not read: `composeFromNotes` owns them, for the preview and the
 * save alike. Reading them here too gave the sheet two buttons that each moved
 * the notes' checklist, counted differently.
 */
export function extractDraftSubtasks(
  draft: { description?: string; acceptance_criteria?: readonly string[] },
): DraftChecklistExtraction {
  // The rows as they stand, not deduped: room is counted from them.
  const existing = (draft.acceptance_criteria ?? []).filter((row) => typeof row === "string" && row.trim());
  const description = typeof draft.description === "string" ? draft.description : "";
  const found = extractSubtasksFromContent(description, { existing });
  return {
    description: found.remainingText,
    subtasks: appendCriteria(draft.acceptance_criteria ?? [], found.subtasks).list,
    extracted: found.subtasks.length + found.duplicates > 0,
    count: found.subtasks.length,
    duplicates: found.duplicates,
    overflow: found.overflow,
  };
}

/** The sentence the sheet shows after a move, naming every outcome that happened. */
export function describeChecklistMove(result: Pick<DraftChecklistExtraction, "count" | "duplicates" | "overflow" | "extracted">): string {
  if (!result.extracted) {
    return result.overflow > 0
      ? `The criteria list is full (${MAX_SUBTASKS}); nothing was moved.`
      : "No checklist found. Write items as “- [ ] item”, or list them under an “Acceptance criteria:” line.";
  }
  // Named for the section the items land in; "subtasks" are the separate
  // child cards further down the sheet.
  const parts = [result.count > 0
    ? `Moved ${result.count} into acceptance criteria`
    : `Removed ${result.duplicates} line${result.duplicates === 1 ? "" : "s"} already in acceptance criteria`];
  if (result.count > 0 && result.duplicates > 0) parts.push(`${result.duplicates} already listed`);
  if (result.overflow > 0) parts.push(`${result.overflow} left in the text (list full)`);
  return parts.join(" · ");
}


/**
 * How an agent handed a task is told to work, as the store's brief renders it.
 *
 * The text is dc-store's own (`dc_store::workbench::AGENT_GUIDANCE`, vendored):
 * every saved brief opens with it, so a copied saved brief, a terminal launch
 * and a managed run all carry the same words, recorded with each attempt. A
 * saved copy therefore adds nothing; only an unsaved draft, which never went
 * through the store, renders the section here. It names the field roles,
 * keeps the author's words intact, sends the agent to the repository's own
 * instructions and code before it writes, points at GitPulse, DevMap and
 * DevCouncil, and states the engineering and verification rules.
 *
 * Named skills are asserted against the directories that ship them, so a
 * skill added or renamed cannot quietly fall out of this text.
 */
export const AGENT_GUIDANCE_SECTION = `## Agent guidance\n${AGENT_GUIDANCE.trim()}`;

/**
 * The completion rule a copy states for itself. The store's guidance names no
 * completion step, because a launched attempt's permission mode decides it;
 * a clipboard copy has no mode, so it says when finishing is allowed here.
 */
export const AGENT_COPY_COMPLETION =
  "When the brief names a saved task on its Task: line and every acceptance criterion is met with your verification passing, mark it done with gitpulse_complete_task: that id, the repo_path, and a short summary of what changed and how you verified it. If anything is unfinished or needs a person's judgement, use status review and say what remains. Never mark done work you did not finish.";

const TITLE_CAP = 300;
const NOTES_CAP = 65_536;
const WEAK_TITLE_CLAUSE = /^(?:\d+|[A-Za-z]{1,4}|[A-Za-z](?:\.[A-Za-z])+)$/;
/**
 * Control characters, plus the invisible ones that survive a copy from a chat
 * or a web page: zero-width spaces and joiners, the byte-order mark, the word
 * joiner, and the bidi embeddings and isolates that can make a title read
 * differently from the text it holds.
 */
const INVISIBLE = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F\u0085\u200B-\u200D\uFEFF\u2060\u202A-\u202E\u2066-\u2069]/g;
/** Unicode's line and paragraph separators, which `\n` splitting would miss. */
const LINE_SEPARATORS = /\u2028|\u2029/g;
const BARE_URL = /^https?:\/\/\S+$/i;
const TITLE_CONTENT = /[\p{L}\p{N}\p{Extended_Pictographic}]/u;
const TITLE_LEAD_IN = /^(?:#{1,6}\s+|[-*+]\s+(?:\[[ xX]\]\s+)?|>\s+)/;
const TITLE_QUOTES: ReadonlyArray<readonly [string, string]> = [['"', '"'], ["'", "'"], ["“", "”"], ["‘", "’"], ["«", "»"]];

/** A line without its heading, list or quote lead-ins, and one pair of wrapping quotes. */
function stripTitleMarkup(line: string): string {
  let next = line.trim();
  for (let i = 0; i < 3 && TITLE_LEAD_IN.test(next); i++) next = next.replace(TITLE_LEAD_IN, "").trim();
  for (const [open, close] of TITLE_QUOTES) {
    if (next.length > open.length + close.length && next.startsWith(open) && next.endsWith(close)) {
      return next.slice(open.length, next.length - close.length).trim();
    }
  }
  return next;
}

/** A heading that only names a part of the brief ("Description", "Steps:") is not what the task is about. */
const SECTION_LABEL = /^(?:description|summary|context|background|overview|details|notes?|problem|issue|bug|steps(?:\s+to\s+reproduce)?|repro(?:duction)?(?:\s+steps)?|expected(?:\s+behaviou?r)?|actual(?:\s+behaviou?r)?|environment)$/i;

/** A pasted link, a line of punctuation, or a bare section label names nothing; the next line may. */
function canTitle(line: string): boolean {
  if (!TITLE_CONTENT.test(line) || BARE_URL.test(line)) return false;
  const { name, colon, heading } = headingName(line);
  const label = heading || colon || name === line.trim();
  return !(label && (SECTION_LABEL.test(name) || CHECKLIST_NAME.test(name)));
}

function isWeakTitleClause(clause: string): boolean {
  const body = clause.trim().replace(/[.!?]+$/u, "").trim();
  return !body || WEAK_TITLE_CLAUSE.test(body);
}

function firstStrongClause(line: string): string {
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (ch !== "." && ch !== "!" && ch !== "?") continue;
    if (i + 1 < line.length && !/\s/u.test(line[i + 1]!)) continue;
    const clause = line.slice(0, i + 1).trim();
    if (isWeakTitleClause(clause)) continue;
    return clause;
  }
  return line;
}

function titleSource(text: string): string {
  const lines = text.split(/\n/);
  for (const raw of lines) {
    const line = stripTitleMarkup(raw.trim());
    if (!line || !canTitle(line)) continue;
    const clause = firstStrongClause(line);
    if (!isWeakTitleClause(clause)) return clause;
  }
  // Only weak clauses: the first line that names anything, even weakly. Notes
  // that are nothing but links have no title, and the caller says so.
  return lines.map((raw) => stripTitleMarkup(raw.trim())).find(canTitle) ?? "";
}

export function sanitizeNotes(value: unknown, cap = NOTES_CAP): string {
  if (typeof value !== "string") return "";
  const cleaned = value.replace(INVISIBLE, "").replace(LINE_SEPARATORS, "\n").trim();
  if (!cleaned) return "";
  const limit = Number.isSafeInteger(cap) && cap >= 32 ? cap : NOTES_CAP;
  return [...cleaned].slice(0, limit).join("");
}

/** First strong line or sentence of notes, skipping list markers and abbreviations. */
export function titleFromNotes(notes: unknown, cap = TITLE_CAP): string {
  const limit = Number.isSafeInteger(cap) && cap >= 16 ? cap : TITLE_CAP;
  const text = sanitizeNotes(notes, NOTES_CAP);
  if (!text) return "";
  const source = titleSource(text);
  const characters = [...source];
  return characters.length > limit ? characters.slice(0, limit - 1).join("") + "…" : source;
}

export function applyNotesToDraft(
  draft: Pick<TaskDraft, "title" | "description">,
  notes: unknown,
): { title: string; description: string; extracted: boolean } {
  // Preserve all entered evidence; the save boundary reports oversized input.
  const text = sanitizeNotes(notes, typeof notes === "string" ? Math.max(NOTES_CAP, notes.length) : NOTES_CAP);
  if (!text) {
    return {
      title: typeof draft.title === "string" ? draft.title : "",
      description: typeof draft.description === "string" ? draft.description : "",
      extracted: false,
    };
  }
  const existingTitle = typeof draft.title === "string" ? draft.title.trim() : "";
  const title = existingTitle || titleFromNotes(text) || "Draft from notes";
  const description = typeof draft.description === "string" ? draft.description : "";
  return { title, description: description.trim() && description.trim() !== text ? `${description}\n\n${text}` : text, extracted: true };
}

/**
 * Whether line `at` is a paragraph or heading of its own, rather than one step
 * of a list or the first line of a sentence that continues below it.
 */
function standsAlone(lines: readonly string[], at: number): boolean {
  const line = lines[at]!.trim();
  if (isListText(line)) return false;
  if (headingName(line).heading) return true;
  const next = lines[at + 1]?.trim();
  if (!next) return true;
  return !carriesOn(line, next);
}

/** What pasted notes turn into: the preview the sheet draws and the write a save makes. */
export interface NotesComposition {
  title: string;
  description: string;
  acceptance_criteria: string[];
  /** Left over after applying: empty once applied, so a later save cannot apply them twice. */
  notes: string;
  /** Whether the notes held anything to apply. */
  extracted: boolean;
  /** The title was empty and now comes from the notes. */
  titleFromNotes: boolean;
  /** Criteria the notes' checklist adds to the list. */
  addedCriteria: string[];
  /** The prose the description gains; empty when the notes were only a checklist. */
  addedText: string;
}

/**
 * Turn pasted notes into title, description and acceptance criteria — once.
 *
 * One function draws the preview and makes the write, so what the sheet says
 * a save will do is what the save does (the same contract as `parseQuickAdd`).
 * The checklist in the notes becomes criteria rather than a second copy inside
 * the description; the rest of the notes is kept word for word, after what the
 * description already holds. An empty title takes the notes' first strong line.
 */
export function composeFromNotes(
  draft: Pick<TaskDraft, "title" | "description"> & { acceptance_criteria?: readonly string[] },
  notes: unknown,
): NotesComposition {
  const title = typeof draft.title === "string" ? draft.title : "";
  const description = typeof draft.description === "string" ? draft.description : "";
  // Preserve all entered evidence; the save boundary reports oversized input.
  const text = sanitizeNotes(notes, typeof notes === "string" ? Math.max(NOTES_CAP, notes.length) : NOTES_CAP);
  if (!text) {
    return { title, description, acceptance_criteria: appendCriteria(draft.acceptance_criteria ?? [], []).list, notes: "", extracted: false, titleFromNotes: false, addedCriteria: [], addedText: "" };
  }
  const checklist = extractSubtasksFromContent(text, { existing: draft.acceptance_criteria ?? [] });
  const derived = !title.trim();
  let prose = checklist.remainingText.trim();
  // A title that is a whole line of the notes moves there instead of being
  // written twice. The words are kept: they are the title. A title that is
  // only the first sentence of a longer line, or was cut at the cap, leaves
  // that line where it is, because the description is then the only place
  // holding the rest of it. So does a line that is not a paragraph of its own:
  // a list item (the first step of a list) or the first line of a paragraph
  // that wraps onto the next.
  const lifted = derived ? titleFromNotes(prose) : "";
  if (lifted) {
    const lines = prose.split("\n");
    const at = lines.findIndex((line) => stripTitleMarkup(line.trim()) === lifted);
    if (at >= 0 && standsAlone(lines, at)) {
      // A bare label such as "## Description" that only introduced the title
      // line has nothing left to introduce at the top of a description.
      const before = lines.slice(0, at).filter((line) => line.trim());
      const label = before.length === 1 && SECTION_LABEL.test(headingName(before[0]!).name);
      prose = [...(label ? [] : lines.slice(0, at)), ...lines.slice(at + 1)].join("\n").trim();
    }
  }
  const applied = applyNotesToDraft({ title: lifted || title, description }, prose);
  // Notes that are only a checklist are named by their first item, never by the
  // "Subtasks:" heading above it.
  const nextTitle = lifted
    || (applied.extracted ? applied.title : "")
    || (title.trim() ? title : titleFromNotes(checklist.subtasks[0] ?? "") || titleFromNotes(text) || "Draft from notes");
  return {
    title: nextTitle,
    description: applied.description,
    acceptance_criteria: appendCriteria(draft.acceptance_criteria ?? [], checklist.subtasks).list,
    notes: "",
    extracted: true,
    titleFromNotes: derived,
    addedCriteria: checklist.subtasks,
    // Empty when the description already held these words, so the preview never
    // promises a line the save will not add.
    addedText: applied.description !== description ? prose : "",
  };
}

export function canAskManvi(draft: {
  title?: unknown;
  description?: unknown;
  repository_ids?: unknown;
  acceptance_criteria?: unknown;
}, notes?: unknown): string | null {
  const repos = Array.isArray(draft.repository_ids)
    ? draft.repository_ids.filter((id) => typeof id === "string" && id.length > 0)
    : [];
  if (!repos.length) return "Link a repository before asking Manvi.";
  // Measured on what the save would write — the same composition — so a paste
  // that is mostly a checklist is not refused here and then saved fine.
  const next = composeFromNotes(
    {
      title: typeof draft.title === "string" ? draft.title : "",
      description: typeof draft.description === "string" ? draft.description : "",
      acceptance_criteria: Array.isArray(draft.acceptance_criteria) ? draft.acceptance_criteria.filter((item): item is string => typeof item === "string") : [],
    },
    notes,
  );
  if (!next.title.trim()) return "Add a title, or describe what you need, before asking Manvi.";
  if (new TextEncoder().encode(next.description).length > NOTES_CAP) return "Description and notes exceed 64 KB. Shorten them before saving or asking Manvi.";
  return null;
}

export interface DraftAgentCopy {
  title: string;
  description: string;
  kind?: string;
  status?: TaskStatus;
  priority?: number;
  owner?: string | null;
  labels?: readonly string[];
  acceptance_criteria?: readonly string[];
  repositoryNames?: readonly string[];
  logs?: string;
}

/**
 * The description as the agent will read it, with any cut announced.
 *
 * An unsaved draft can hold more than a saved one: `applyNotesToDraft` raises
 * its own cap on purpose so nothing the author typed is lost before the save
 * boundary reports it, while a saved brief's description is bounded at
 * `NOTES_CAP` by the store. So this is the one path where the copy can be
 * shorter than the draft it came from.
 *
 * Bounding the packet is right; bounding it silently is not. A copy that stops
 * mid-sentence with no marker reads to the agent as the whole description, and
 * it answers as though it had seen the rest. Same contract as the model
 * budgeters in `ai/prompt.rs`: cut, and say so in the text that was cut.
 */
function copyDescription(value: unknown): string {
  const text = sanitizeNotes(value, Number.MAX_SAFE_INTEGER);
  const characters = [...text];
  if (characters.length <= NOTES_CAP) return text;
  return `${characters.slice(0, NOTES_CAP).join("")}\n\n[description truncated: ${NOTES_CAP} of ${characters.length} characters shown]`;
}

export function formatDraftAgentCopy(draft: DraftAgentCopy): string | null {
  const title = displayTitle(draft.title, TITLE_CAP);
  const description = copyDescription(draft.description);
  if (title === "(untitled)" && !description) return null;
  const criteria = (draft.acceptance_criteria ?? []).map((item) => item.trim()).filter(Boolean);
  const labels = (draft.labels ?? []).map((item) => item.trim()).filter(Boolean);
  const repos = (draft.repositoryNames ?? []).map((item) => item.trim()).filter(Boolean);
  const status = draft.status && STATUS_LABELS[draft.status] ? STATUS_LABELS[draft.status] : "Unspecified";
  const priority = priorityLabel(draft.priority);
  const lines = [
    AGENT_COPY_COMPLETION,
    "",
    "# Unsaved GitPulse task draft",
    "This has not been saved. Treat these fields as the author's current intent, not a stored revision.",
    "",
    AGENT_GUIDANCE_SECTION,
    "",
    "## Title",
    title,
    "",
    `Type: ${draft.kind?.trim() || "Unspecified"}`,
    `Status: ${status}`,
    `Priority: ${priority}`,
    `Owner: ${draft.owner?.trim() || "Unassigned"}`,
    `Labels: ${labels.length ? labels.join(", ") : "None"}`,
    "",
    "## Repositories",
    repos.length ? repos.map((name) => `- ${name}`).join("\n") : "None linked yet.",
    "",
    "## Description",
    description || "(none)",
    "",
    "## Acceptance criteria",
    criteria.length ? criteria.map((item) => `- [ ] ${item}`).join("\n") : "No acceptance criteria recorded.",
  ];
  const logs = formatLogsSection(draft.logs);
  if (logs) lines.push("", logs);
  return lines.join("\n");
}

export function wrapSavedBriefForAgent(markdown: unknown): string | null {
  if (typeof markdown !== "string") return null;
  const body = markdown.replace(/\u0000/g, "").trim();
  if (!body || body.length > 2 * 1024 * 1024) return null;
  // The saved brief already opens with the agent guidance; adding it again
  // would hand the agent two copies to reconcile.
  return `${AGENT_COPY_COMPLETION}\n\n${body}`;
}

export function suggestionDiffers(current: unknown, proposed: unknown): boolean {
  const a = typeof current === "string" ? current.trim() : "";
  const b = typeof proposed === "string" ? proposed.trim() : "";
  if (!b) return false;
  return a !== b;
}

export function joinAgentCopies(parts: readonly string[], limit = MAX_AGENT_COPY_TASKS): string | null {
  const cleaned = (Array.isArray(parts) ? parts : [])
    .map((part) => (typeof part === "string" ? part.trim() : ""))
    .filter(Boolean)
    .slice(0, Number.isSafeInteger(limit) && limit > 0 ? Math.min(limit, 50) : MAX_AGENT_COPY_TASKS);
  if (!cleaned.length) return null;
  return cleaned.join("\n\n---\n\n");
}
