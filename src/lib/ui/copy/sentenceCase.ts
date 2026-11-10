/**
 * Sentence case for interface labels (UI_RULES §3: buttons, menus, tabs,
 * headings and tooltips are sentence case; proper nouns keep their case).
 *
 * This is a conservative normaliser, not a grammar engine. It lowercases
 * ordinary words after the first, and leaves alone anything that is plausibly
 * meant to be capitalised. The rules, in the order they apply:
 *
 *  1. A multi-word proper noun ("Claude Code") is kept whole, in its canonical
 *     spelling.
 *  2. A keyboard shortcut ("Ctrl+K", "⌘K") is kept verbatim.
 *  3. A single-word proper noun is restored to its canonical spelling, even
 *     when it was typed in the wrong case ("github" becomes "GitHub"). The
 *     ordinary-word nouns in `AMBIGUOUS_PROPER_NOUNS` ("Windows", "Git") are
 *     only kept when already capitalised, never repaired from lowercase.
 *  4. An all-caps token ("PR", "S3", "URL") is an acronym and is kept.
 *     Exception: when a whole label is shouting (two or more words, all caps,
 *     at least one of five letters or more: "DELETE ALL") only the entries of
 *     `KNOWN_ACRONYMS` survive, so "OPEN PR" becomes "Open PR".
 *  5. A token with a capital after its first letter and a lowercase letter
 *     ("OAuth", "iPhone", "PRs") is a name and is kept.
 *  6. A single capital letter that is not the first word is kept ("Press K",
 *     "Plan B"), except "A", which is read as the article ("Create A branch"
 *     becomes "Create a branch"; the cost is "Plan A" becoming "Plan a").
 *  7. Everything else is lowercased, and the first token of the label gets its
 *     first letter capitalised.
 *
 * Known limits: a name that is not in the list and has only a leading capital
 * is lowercased mid-label ("Kingfisher" becomes "kingfisher"), and a label
 * that starts with a lowercase name outside the list ("npm") gets its first
 * letter capitalised. Pass extra nouns through `properNouns` when a surface
 * needs them.
 */

import { fileManagerName, osDisplayName, systemSettingsName } from "../platformCopy";

/**
 * Proper nouns that keep their case in a label. Spaces are allowed
 * ("Claude Code"); the match is case-insensitive and whole-word, and the
 * entry's own spelling is the canonical output.
 */
export const PROPER_NOUNS: readonly string[] = [
  "GitPulse",
  "GitHub",
  "Manvi",
  "DevMap",
  "DevCouncil",
  "Git",
  // Host vocabulary comes from platformCopy, the one module allowed to spell
  // it (scripts/platform-vocabulary-contract.test.ts).
  osDisplayName("macos"),
  osDisplayName("windows"),
  osDisplayName("linux"),
  fileManagerName("macos"),
  fileManagerName("windows"),
  systemSettingsName("macos"),
  systemSettingsName("windows"),
  "Claude",
  "Claude Code",
  "Codex",
  "Grok",
  "Antigravity",
  "Firebase",
  "Tauri",
  "MCP",
];

/**
 * The entries of `PROPER_NOUNS` that are also ordinary words or commands:
 * "Windows" (a terminal window, many windows) and "Git" (the `git` command).
 * They are kept when written with their capital but are NOT repaired from
 * lowercase, so "Close all windows" and "Run git status" stay as typed. The
 * cost is that a Title Case label is not caught on these words ("Close all
 * Windows" passes). Applies to a call's `properNouns` by exact spelling.
 */
export const AMBIGUOUS_PROPER_NOUNS: readonly string[] = ["Windows", "Git"];

/**
 * All-caps words that are acronyms rather than shouting. Only consulted when a
 * whole label is in capitals (rule 4); in any other label an all-caps token
 * is kept without a list.
 */
export const KNOWN_ACRONYMS: readonly string[] = [
  "AI",
  "API",
  "CI",
  "CLI",
  "CPU",
  "CSV",
  "DNS",
  "GPU",
  "HTTP",
  "HTTPS",
  "ID",
  "IDE",
  "JSON",
  "LFS",
  "MCP",
  "OS",
  "PDF",
  "PR",
  "PTY",
  "SHA",
  "SQL",
  "SSH",
  "SSO",
  "TLS",
  "UI",
  "URL",
  "UUID",
  "VM",
  "XML",
  "YAML",
];

const TOKEN = /[\p{L}\p{N}]+(?:['’][\p{L}]+)*/gu;
const MODIFIER_KEY = /(?:^|\+)(?:cmd|command|ctrl|control|alt|option|opt|shift|meta|win|super|fn)(?:\+|$)/i;
const SHORTCUT_GLYPH = /[⌘⌥⌃⇧⎋↵⇥⌫␣]/u;
const EDGE_PUNCTUATION = /^[("'[“‘]+|[)"'\].,;:!?”’]+$/g;

interface Span {
  readonly start: number;
  readonly end: number;
  readonly text: string;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function hasLetter(text: string): boolean {
  return /\p{L}/u.test(text);
}

function isUpper(text: string): boolean {
  return text !== text.toLowerCase() && text === text.toUpperCase();
}

/** All-caps and at least two characters ("PR", "S3"); not "A" and not "3". */
function isAcronymShaped(token: string): boolean {
  return token.length >= 2 && hasLetter(token) && token === token.toUpperCase();
}

/** A capital after the first character, with some lowercase present ("OAuth"). */
function hasInternalCapital(token: string): boolean {
  if (token === token.toUpperCase() || token === token.toLowerCase()) return false;
  return Array.from(token)
    .slice(1)
    .some((char) => isUpper(char));
}

function capitalizeFirst(word: string): string {
  const [first, ...rest] = Array.from(word);
  if (first === undefined) return word;
  const upper = first.toUpperCase();
  // "ß".toUpperCase() is "SS": a one-letter rule must not grow the word.
  return (upper.length === first.length ? upper : first) + rest.join("");
}

function isShoutingLabel(label: string): boolean {
  const tokens = (label.match(TOKEN) ?? []).filter((token) => Array.from(token).filter((c) => hasLetter(c)).length >= 2);
  if (tokens.length < 2) return false;
  if (!tokens.every((token) => token === token.toUpperCase())) return false;
  return tokens.some((token) => Array.from(token).filter((c) => hasLetter(c)).length >= 5);
}

function protectedSpans(label: string, phrases: readonly string[]): Span[] {
  const spans: Span[] = [];
  for (const phrase of phrases) {
    const pattern = new RegExp(
      `(?<![\\p{L}\\p{N}])${escapeRegExp(phrase).replace(/\s+/g, "\\s+")}(?![\\p{L}\\p{N}])`,
      "giu",
    );
    for (const match of label.matchAll(pattern)) {
      spans.push({ start: match.index, end: match.index + match[0].length, text: phrase });
    }
  }
  for (const match of label.matchAll(/\S+/g)) {
    const word = match[0];
    const core = word.replace(EDGE_PUNCTUATION, "");
    const isChord = core.includes("+") && MODIFIER_KEY.test(core);
    if (isChord || SHORTCUT_GLYPH.test(word)) {
      spans.push({ start: match.index, end: match.index + word.length, text: word });
    }
  }
  spans.sort((a, b) => a.start - b.start || b.end - a.end);
  const kept: Span[] = [];
  let cursor = 0;
  for (const span of spans) {
    if (span.start < cursor) continue;
    kept.push(span);
    cursor = span.end;
  }
  return kept;
}

/**
 * Rewrite `label` into sentence case. Whitespace, punctuation and the `…`
 * character are untouched. Pure and idempotent: applying it twice gives the
 * same result as applying it once.
 *
 * `properNouns` replaces `PROPER_NOUNS` for this call; pass
 * `[...PROPER_NOUNS, "Kingfisher"]` to extend rather than replace. An entry
 * that is blank throws, because a blank phrase would match everywhere.
 */
export function toSentenceCase(label: string, properNouns: readonly string[] = PROPER_NOUNS): string {
  const nouns = properNouns.map((noun) => noun.trim());
  if (nouns.some((noun) => noun.length === 0)) {
    throw new RangeError("toSentenceCase: a proper noun must not be blank");
  }
  const words = new Map<string, string>();
  const keptWhenCapitalized = new Set<string>();
  const phrases: string[] = [];
  for (const noun of nouns) {
    if (/\s/.test(noun)) phrases.push(noun);
    else if (AMBIGUOUS_PROPER_NOUNS.includes(noun)) keptWhenCapitalized.add(noun);
    else words.set(noun.toLowerCase(), noun);
  }
  const acronyms = new Set(KNOWN_ACRONYMS.map((acronym) => acronym.toLowerCase()));
  const shouting = isShoutingLabel(label);
  let firstDone = false;

  const rewriteGap = (gap: string): string =>
    gap.replace(TOKEN, (token) => {
      const isFirst = !firstDone;
      firstDone = true;
      const lower = token.toLowerCase();
      const noun = words.get(lower);
      if (noun !== undefined) return noun;
      if (keptWhenCapitalized.has(token)) return token;
      if (isAcronymShaped(token) && (!shouting || acronyms.has(lower))) return token;
      if (hasInternalCapital(token)) return token;
      if (token.length === 1 && isUpper(token) && !isFirst && token !== "A") return token;
      return isFirst ? capitalizeFirst(lower) : lower;
    });

  let result = "";
  let cursor = 0;
  for (const span of protectedSpans(label, phrases)) {
    result += rewriteGap(label.slice(cursor, span.start)) + span.text;
    firstDone = true;
    cursor = span.end;
  }
  return result + rewriteGap(label.slice(cursor));
}

/** True when `toSentenceCase` would leave `label` unchanged. An empty label is trivially so. */
export function isSentenceCase(label: string, properNouns: readonly string[] = PROPER_NOUNS): boolean {
  return toSentenceCase(label, properNouns) === label;
}
