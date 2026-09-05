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
 * The diff view had none of these: it printed the raw line in one colour with
 * the word-diff segments as the only structure, while the file viewer beside
 * it has had syntax highlighting the whole time, from a tokenizer that ships
 * in this repository. Two readings of the same file should not disagree about
 * what a keyword looks like.
 */

import { tokenizeLine, type SupportedLanguage, type SyntaxToken } from "../files/syntaxHighlight";
import type { DiffChunkKind, DiffSegment } from "./wordDiff";

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
 * Splits `text` at every boundary the three layers introduce.
 *
 * Adjacent slices carrying identical answers are merged, so a plain line of
 * unchanged code renders as one span rather than as one span per token when
 * the language is not highlighted.
 */
export function composeSpans(
  text: string,
  language: SupportedLanguage,
  segments: readonly DiffSegment[] | undefined,
  changedKind: DiffChunkKind,
  matches: readonly Range[] = [],
  options: { syntax?: boolean } = {},
): DiffSpan[] {
  if (text.length === 0) return [];

  const wantSyntax =
    options.syntax !== false && language !== "plaintext" && text.length <= MAX_HIGHLIGHT_CHARS;
  const tokens: SyntaxToken[] = wantSyntax
    ? tokenizeLine(text, language)
    : [{ text, type: "text" }];

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
