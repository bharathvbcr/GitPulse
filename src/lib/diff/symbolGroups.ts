/**
 * Group unified-diff hunks under DevMap symbol spans.
 *
 * This is not the suspects cone (`CodeintelTouchedSymbol` / `dc-regress`).
 * It only overlaps hunk line ranges with `symbols_for_file` spans.
 */

export interface SymbolSpan {
  symbol_name: string;
  span_start_line: number;
  span_end_line: number;
}

export interface HunkLineRange {
  /** Inclusive start on the old (HEAD / parent) side; 0 means none. */
  old_start: number;
  old_lines: number;
  /** Inclusive start on the new side; 0 means none. */
  new_start: number;
  new_lines: number;
  /** Stable key for the hunk (e.g. outline index). */
  key: string;
}

export interface SymbolHunkGroup {
  /** Empty string means no symbol spanned this hunk. */
  symbol_name: string;
  hunk_keys: string[];
}

function inclusiveRange(start: number, count: number): { start: number; end: number } | null {
  if (start <= 0 || count <= 0) return null;
  return { start, end: start + count - 1 };
}

function overlaps(
  a: { start: number; end: number },
  symbol: SymbolSpan,
): boolean {
  if (symbol.span_start_line <= 0 || symbol.span_end_line <= 0) return false;
  return a.start <= symbol.span_end_line && symbol.span_start_line <= a.end;
}

/**
 * Pick the tightest (smallest) symbol whose span overlaps the hunk's old or
 * new line range. Nested symbols prefer the inner one.
 */
export function symbolForHunk(
  hunk: HunkLineRange,
  symbols: readonly SymbolSpan[],
): string {
  const oldRange = inclusiveRange(hunk.old_start, hunk.old_lines);
  const newRange = inclusiveRange(hunk.new_start, hunk.new_lines);
  let best: SymbolSpan | null = null;
  let bestSize = Number.POSITIVE_INFINITY;
  for (const symbol of symbols) {
    const hit =
      (oldRange !== null && overlaps(oldRange, symbol)) ||
      (newRange !== null && overlaps(newRange, symbol));
    if (!hit) continue;
    const size = symbol.span_end_line - symbol.span_start_line;
    if (size < bestSize) {
      best = symbol;
      bestSize = size;
    }
  }
  return best?.symbol_name ?? "";
}

/**
 * Group hunks in outline order under their spanning symbol.
 *
 * Adjacent hunks with the same symbol share a group. Hunks with no symbol
 * each get their own empty-named group so the caller can still render them.
 */
export function groupHunksBySymbol(
  hunks: readonly HunkLineRange[],
  symbols: readonly SymbolSpan[],
): SymbolHunkGroup[] {
  if (hunks.length === 0) return [];
  const groups: SymbolHunkGroup[] = [];
  for (const hunk of hunks) {
    const name = symbolForHunk(hunk, symbols);
    const last = groups[groups.length - 1];
    if (last && last.symbol_name === name && name !== "") {
      last.hunk_keys.push(hunk.key);
    } else {
      groups.push({ symbol_name: name, hunk_keys: [hunk.key] });
    }
  }
  return groups;
}

/**
 * Map from hunk key → symbol label to show beside the hunk chrome.
 * Empty string means "no symbol group for this hunk".
 */
export function hunkSymbolLabels(
  hunks: readonly HunkLineRange[],
  symbols: readonly SymbolSpan[],
): Map<string, string> {
  const out = new Map<string, string>();
  for (const hunk of hunks) {
    out.set(hunk.key, symbolForHunk(hunk, symbols));
  }
  return out;
}
