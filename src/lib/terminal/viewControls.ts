import { isImeComposition } from "../keyboard/imeGuard";

export const TERMINAL_FONT_MIN = 10;
export const TERMINAL_FONT_MAX = 24;
export const TERMINAL_FONT_DEFAULT = 12;
export const SEARCH_HIGHLIGHT_LIMIT = 1000;
export const SEARCH_QUERY_LIMIT = 256;

export function clampTerminalFontSize(size: number): number {
  if (!Number.isFinite(size)) return TERMINAL_FONT_DEFAULT;
  return Math.max(TERMINAL_FONT_MIN, Math.min(TERMINAL_FONT_MAX, Math.round(size)));
}

export type TerminalViewChord = "find" | "zoom-in" | "zoom-out" | "zoom-reset";

/** Leave readline's Ctrl+F and the app's Ctrl+K/Command+K palette intact. */
export function terminalViewChord(event: {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
}): TerminalViewChord | null {
  if (isImeComposition(event) || event.altKey) return null;
  const command = event.metaKey && !event.ctrlKey;
  const controlShift = event.ctrlKey && event.shiftKey && !event.metaKey;
  if (!command && !controlShift) return null;
  switch (event.key.toLowerCase()) {
    case "f": return "find";
    case "=":
    case "+": return "zoom-in";
    case "_":
    case "-": return "zoom-out";
    case ")":
    case "0": return "zoom-reset";
    default: return null;
  }
}

/** The addon caps highlighted results; a capped count is never a total. */
export function terminalSearchSummary(index: number, count: number): string {
  if (count === 0) return "No matches";
  if (count >= SEARCH_HIGHLIGHT_LIMIT) return `${SEARCH_HIGHLIGHT_LIMIT}+ matches`;
  if (index < 0) return `${count} matches`;
  return `${index + 1} of ${count}`;
}
