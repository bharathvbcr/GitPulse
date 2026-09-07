/**
 * Three highlights over one line of code, composed into one span list.
 *
 * A diff row wants to say three things at once: what the code *is* (syntax),
 * what the commit *changed* (the intra-line word diff), and what the reader
 * is *looking for* (the search hit). Rendering them as nested elements does
 * not work — the three do not nest, they overlap at arbitrary offsets — so
 * they are flattened here into a single sequence of spans, each carrying all
 * three answers.
 *
 * Syntax colouring has one owner and two backends. MarkDev's tree-sitter
 * grammars cover six languages; the regex tokenizer in `syntaxHighlight.ts`
 * covers the rest. Callers highlight a whole file once (Rust) and pass the
 * per-line tokens in, or let {@link resolveLineTokens} pick the sync
 * tokenizer when no document-level result is available.
 */

import {
  tokenizeLine,
  type SupportedLanguage,
  type SyntaxToken,
  type TokenType,
} from "../files/syntaxHighlight";
import { syntaxHighlight, type TreeSitterSpan } from "../syntax/client";
import type { DiffChunkKind, DiffSegment } from "./wordDiff";

export type { TreeSitterSpan };

export interface Range {
  start: number;
  /** Exclusive. */
  end: number;
}

export interface DiffSpan {
  text: string;
  /** Syntax token type, so the caller maps it through its own palette. */
  token: SyntaxToken["type"];
  /** Inside a word-diff segment the commit added or removed. */
  changed: boolean;
  /** Inside a search hit. */
  match: boolean;
}

/**
 * Above this many characters a line is not read, it is scrolled past, and
 * tokenizing it on every frame of a virtual scroll costs more than the
 * highlight is worth. Minified bundles and base64 blobs live here.
 */
export const MAX_HIGHLIGHT_CHARS = 2_000;

/**
 * Languages MarkDev's tree-sitter layer can highlight.
 *
 * Mirrors the keys in markdev's `configurations()` — keep in sync when a
 * grammar is added upstream. Everything else stays on the regex tokenizer.
 */
export const TREE_SITTER_LANGUAGES: ReadonlySet<SupportedLanguage> = new Set([
  "rust",
  "javascript",
  "typescript",
  "python",
  "json",
  "shell",
]);

/** Whether `language` should be highlighted in Rust when a whole file is available. */
export function usesTreeSitter(language: SupportedLanguage): boolean {
  return TREE_SITTER_LANGUAGES.has(language);
}

/** Maps a MarkDev highlight kind name onto the shared TokenType palette. */
export function tokenTypeFromKind(kind: string): TokenType {
  switch (kind) {
    case "keyword":
      return "keyword";
    case "string":
      return "string";
    case "number":
      return "number";
    case "comment":
      return "comment";
    case "function":
      return "function";
    case "type":
      return "type";
    case "variable":
      return "variable";
    case "operator":
      return "operator";
    case "punctuation":
      return "punctuation";
    case "attribute":
      return "attribute";
    default:
      return "text";
  }
}

/**
 * Character ranges the word diff marks as changed on this side of a pair.
 *
 * Returns an empty list when the segments do not reconstruct `text` exactly.
 * That should not happen, but the alternative to checking is painting the
 * "changed" background over offsets that belong to different characters —
 * a highlight that is confidently wrong is worse than none.
 */
export function segmentRanges(
  text: string,
  segments: readonly DiffSegment[] | undefined,
  kind: DiffChunkKind,
): Range[] {
  if (!segments || segments.length === 0) return [];
  const ranges: Range[] = [];
  let offset = 0;
  for (const segment of segments) {
    const length = segment.text.length;
    if (segment.kind === kind && length > 0) {
      ranges.push({ start: offset, end: offset + length });
    }
    offset += length;
  }
  if (offset !== text.length) return [];
  return ranges;
}

/** Merges overlapping/adjacent ranges so boundary collection stays linear. */
export function normalizeRanges(ranges: readonly Range[]): Range[] {
  const usable = ranges
    .filter((range) => Number.isFinite(range.start) && Number.isFinite(range.end) && range.end > range.start)
    .map((range) => ({ start: Math.max(0, range.start), end: Math.max(0, range.end) }))
    .sort((a, b) => a.start - b.start || a.end - b.end);
  const merged: Range[] = [];
  for (const range of usable) {
    const last = merged[merged.length - 1];
    if (last && range.start <= last.end) last.end = Math.max(last.end, range.end);
    else merged.push({ ...range });
  }
  return merged;
}

function coveredBy(ranges: readonly Range[], start: number, cursor: { index: number }): boolean {
  while (cursor.index < ranges.length && ranges[cursor.index].end <= start) cursor.index += 1;
  const range = ranges[cursor.index];
  return !!range && range.start <= start;
}

/**
 * Turns UTF-16 highlight spans covering `code` into a contiguous token list
 * that reproduces `code` exactly — gaps between spans become `text` tokens.
 */
export function tokensFromSpans(code: string, spans: readonly TreeSitterSpan[]): SyntaxToken[] {
  if (code.length === 0) return [];
  if (spans.length === 0) return [{ text: code, type: "text" }];

  const ordered = [...spans]
    .filter((span) => span.end > span.start)
    .sort((a, b) => a.start - b.start || a.end - b.end);

  const tokens: SyntaxToken[] = [];
  let cursor = 0;
  for (const span of ordered) {
    const start = Math.max(0, Math.min(code.length, span.start));
    const end = Math.max(start, Math.min(code.length, span.end));
    if (start < cursor) continue;
    if (start > cursor) {
      tokens.push({ text: code.slice(cursor, start), type: "text" });
    }
    if (end > start) {
      tokens.push({ text: code.slice(start, end), type: tokenTypeFromKind(span.kind) });
    }
    cursor = end;
  }
  if (cursor < code.length) {
    tokens.push({ text: code.slice(cursor), type: "text" });
  }
  return tokens;
}

/**
 * Slices document-level highlight spans into one token list per line.
 *
 * Line breaks are `\n` only (matching `String.split("\n")` in the viewers).
 * Spans that cross a newline are split so each line's tokens stay local.
 */
export function lineTokensFromSpans(code: string, spans: readonly TreeSitterSpan[]): SyntaxToken[][] {
  const lines = code.split("\n");
  if (lines.length === 0) return [];

  /** Exclusive UTF-16 end offset of each line's content (before its `\n`). */
  const lineEnds: number[] = [];
  let offset = 0;
  for (let i = 0; i < lines.length; i += 1) {
    offset += lines[i].length;
    lineEnds.push(offset);
    if (i < lines.length - 1) offset += 1; // the `\n`
  }

  const perLine: TreeSitterSpan[][] = lines.map(() => []);
  for (const span of spans) {
    if (span.end <= span.start) continue;
    let lineIndex = 0;
    let lineStart = 0;
    for (; lineIndex < lines.length; lineIndex += 1) {
      const lineEnd = lineEnds[lineIndex];
      if (span.start < lineEnd || (span.start === lineEnd && span.start === span.end)) {
        // Span starts on this line (or at EOF on the last empty segment).
        break;
      }
      lineStart = lineEnd + (lineIndex < lines.length - 1 ? 1 : 0);
    }
    if (lineIndex >= lines.length) continue;

    let start = span.start;
    let kind = span.kind;
    while (lineIndex < lines.length && start < span.end) {
      const lineEnd = lineEnds[lineIndex];
      const localStart = Math.max(0, start - lineStart);
      const localEnd = Math.min(lines[lineIndex].length, span.end - lineStart);
      if (localEnd > localStart) {
        perLine[lineIndex].push({ start: localStart, end: localEnd, kind });
      }
      if (span.end <= lineEnd) break;
      lineIndex += 1;
      lineStart = lineEnd + 1;
      start = lineStart;
      kind = span.kind;
    }
  }

  return lines.map((line, i) => tokensFromSpans(line, perLine[i]));
}

/**
 * Sync token resolution for one line.
 *
 * When `documentTokens` is provided (from a whole-file tree-sitter pass),
 * those win. Otherwise the regex tokenizer is used — including for the six
 * tree-sitter languages, because a single line is not a document and a
 * wrong multi-line parse is worse than a weaker per-line colouring.
 */
export function resolveLineTokens(
  text: string,
  language: SupportedLanguage,
  options: { syntax?: boolean; documentTokens?: readonly SyntaxToken[] } = {},
): SyntaxToken[] {
  if (text.length === 0) return [];
  const wantSyntax =
    options.syntax !== false && language !== "plaintext" && text.length <= MAX_HIGHLIGHT_CHARS;
  if (!wantSyntax) return [{ text, type: "text" }];
  if (options.documentTokens) {
    const joined = options.documentTokens.map((t) => t.text).join("");
    return joined === text ? [...options.documentTokens] : tokenizeLine(text, language);
  }
  return tokenizeLine(text, language);
}

/**
 * Splits `text` at every boundary the three layers introduce.
 *
 * `tokens` must already reproduce `text` exactly; if they do not, the line
 * falls back to a single plain token so offsets never desynchronise.
 */
export function composeSpans(
  text: string,
  tokens: readonly SyntaxToken[],
  segments: readonly DiffSegment[] | undefined,
  changedKind: DiffChunkKind,
  matches: readonly Range[] = [],
): DiffSpan[] {
  if (text.length === 0) return [];

  const changedRanges = normalizeRanges(segmentRanges(text, segments, changedKind));
  const matchRanges = normalizeRanges(matches);

  const boundaries = new Set<number>([0, text.length]);
  let offset = 0;
  for (const token of tokens) {
    offset += token.text.length;
    if (offset < text.length) boundaries.add(offset);
  }
  // A tokenizer that does not reproduce the line exactly would desynchronise
  // every span after the first divergence, so fall back to one plain token.
  const tokenList = offset === text.length ? tokens : [{ text, type: "text" as const }];
  if (offset !== text.length) {
    boundaries.clear();
    boundaries.add(0);
    boundaries.add(text.length);
  }
  for (const range of changedRanges) {
    if (range.start > 0 && range.start < text.length) boundaries.add(range.start);
    if (range.end > 0 && range.end < text.length) boundaries.add(range.end);
  }
  for (const range of matchRanges) {
    if (range.start > 0 && range.start < text.length) boundaries.add(range.start);
    if (range.end > 0 && range.end < text.length) boundaries.add(range.end);
  }

  const cuts = [...boundaries].sort((a, b) => a - b);
  const spans: DiffSpan[] = [];
  const changedCursor = { index: 0 };
  const matchCursor = { index: 0 };
  let tokenIndex = 0;
  let tokenEnd = tokenList.length > 0 ? tokenList[0].text.length : text.length;

  for (let i = 0; i < cuts.length - 1; i += 1) {
    const start = cuts[i];
    const end = cuts[i + 1];
    if (end <= start) continue;
    while (tokenIndex < tokenList.length - 1 && tokenEnd <= start) {
      tokenIndex += 1;
      tokenEnd += tokenList[tokenIndex].text.length;
    }
    const span: DiffSpan = {
      text: text.slice(start, end),
      token: tokenList[tokenIndex]?.type ?? "text",
      changed: coveredBy(changedRanges, start, changedCursor),
      match: coveredBy(matchRanges, start, matchCursor),
    };
    const last = spans[spans.length - 1];
    if (last && last.token === span.token && last.changed === span.changed && last.match === span.match) {
      last.text += span.text;
    } else {
      spans.push(span);
    }
  }
  return spans;
}

/**
 * Resolves tokens for a line, then composes them with change/match layers.
 *
 * Prefer this at call sites that do not already hold document-level tokens;
 * pass `options.documentTokens` (or call {@link composeSpans} directly) when
 * a whole-file tree-sitter pass has already sliced the line.
 */
export function composeLineSpans(
  text: string,
  language: SupportedLanguage,
  segments: readonly DiffSegment[] | undefined,
  changedKind: DiffChunkKind,
  matches: readonly Range[] = [],
  options: { syntax?: boolean; documentTokens?: readonly SyntaxToken[] } = {},
): DiffSpan[] {
  return composeSpans(
    text,
    resolveLineTokens(text, language, options),
    segments,
    changedKind,
    matches,
  );
}

/**
 * Highlights a whole document in Rust when a grammar exists.
 *
 * Returns one token list per line, or `null` when the language has no
 * tree-sitter grammar / the IPC call fails — callers then keep the regex
 * tokenizer. Never invents an empty success: failure is `null`.
 */
export async function highlightDocument(
  language: SupportedLanguage,
  code: string,
): Promise<SyntaxToken[][] | null> {
  if (!usesTreeSitter(language) || code.length === 0) return null;
  try {
    const spans = await syntaxHighlight(language, code);
    return lineTokensFromSpans(code, spans);
  } catch {
    return null;
  }
}

/**
 * Search hits that fall inside one line, as ranges relative to the rendered
 * text rather than to the raw diff line.
 *
 * A diff row renders `content.slice(1)` — the `+`/`-`/space marker is drawn
 * as its own column — so a match found at column 4 of the raw line belongs at
 * column 3 of what the reader sees. Getting this wrong shifts every highlight
 * by one character, which looks like an off-by-one in the search itself.
 */
export function shiftMatches(
  matches: readonly { colStart: number; length: number }[],
  markerOffset: number,
  textLength: number,
): Range[] {
  const ranges: Range[] = [];
  for (const match of matches) {
    const start = match.colStart - markerOffset;
    const end = start + match.length;
    if (end <= 0 || start >= textLength) continue;
    ranges.push({ start: Math.max(0, start), end: Math.min(textLength, end) });
  }
  return ranges;
}
