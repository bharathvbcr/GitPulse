import type { CodeintelFileSymbol, CodeintelNeighbors } from "../codeintel/types";

export interface DirectCallerEntry {
  symbol: string;
  qualifiedSymbol: string;
  filePath: string;
}

/**
 * Selects the innermost indexed symbol whose 1-based `[span_start_line, span_end_line]`
 * encloses `line`. Returns `null` when `line` is invalid or outside all indexed spans.
 */
export function pickEnclosingSymbol(
  items: readonly CodeintelFileSymbol[],
  line: number | null | undefined,
): CodeintelFileSymbol | null {
  if (typeof line !== "number" || !Number.isInteger(line) || line < 1 || items.length === 0) {
    return null;
  }
  let best: CodeintelFileSymbol | null = null;
  let bestSpan = Number.POSITIVE_INFINITY;
  for (const item of items) {
    if (item.span_start_line <= line && line <= item.span_end_line) {
      const span = Math.max(0, item.span_end_line - item.span_start_line);
      if (
        best === null ||
        span < bestSpan ||
        (span === bestSpan && item.span_start_line > best.span_start_line)
      ) {
        best = item;
        bestSpan = span;
      }
    }
  }
  return best;
}

function shortSymbolName(qualified: string): string {
  const trimmed = qualified.trim();
  if (!trimmed) return "";
  const sep = trimmed.lastIndexOf("::");
  return sep >= 0 && sep + 2 < trimmed.length ? trimmed.slice(sep + 2) : trimmed;
}

/**
 * Extracts deduplicated direct (`band 1`) callers from `getNeighbors` results,
 * bounded to `maxShown` while reporting the true deduplicated `total`.
 */
export function extractDirectCallers(
  neighbors: readonly CodeintelNeighbors[],
  maxShown = 5,
): { callers: DirectCallerEntry[]; total: number } {
  const seen = new Set<string>();
  const all: DirectCallerEntry[] = [];
  for (const group of neighbors) {
    const edges = group.callers?.items ?? [];
    for (const edge of edges) {
      if (!edge.source_file) continue;
      const qualified = edge.source_symbol || edge.source_file;
      const key = `${edge.source_file}\0${qualified}`;
      if (seen.has(key)) continue;
      seen.add(key);
      all.push({
        symbol: shortSymbolName(qualified) || edge.source_file,
        qualifiedSymbol: qualified,
        filePath: edge.source_file,
      });
    }
  }
  const cap = Math.max(1, maxShown);
  return {
    callers: all.slice(0, cap),
    total: all.length,
  };
}

/**
 * Formats an enclosing symbol + direct caller summary for 1-click handoff into
 * an active coding agent prompt.
 */
export function formatSymbolLensForAgent(options: {
  filePath: string;
  symbol: CodeintelFileSymbol;
  callers: readonly DirectCallerEntry[];
  totalCallers: number;
}): string {
  const { filePath, symbol, callers, totalCallers } = options;
  const span =
    symbol.span_end_line > symbol.span_start_line
      ? `L${symbol.span_start_line}-L${symbol.span_end_line}`
      : `L${symbol.span_start_line}`;
  const callerDetail =
    callers.length > 0
      ? `; direct callers: ${callers.map((c) => `${c.symbol} (${c.filePath})`).join(", ")}${
          totalCallers > callers.length ? ` +${totalCallers - callers.length} more` : ""
        }`
      : "";
  return `@${filePath}#${span} (${symbol.symbol_name}${callerDetail}) `;
}
