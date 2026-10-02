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

/**
 * Leave readline's Ctrl+F and Command+K intact. On macOS the view chords are
 * Command chords only: Ctrl+Shift+- is `^_`, readline's undo, and a Mac
 * keyboard reaches find and zoom with Command anyway. Elsewhere Control+Shift
 * is the terminal convention, because plain Control belongs to the shell.
 */
export function terminalViewChord(event: {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
}, os?: string): TerminalViewChord | null {
  if (isImeComposition(event) || event.altKey) return null;
  const command = event.metaKey && !event.ctrlKey;
  const controlShift = os !== "macos" && event.ctrlKey && event.shiftKey && !event.metaKey;
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

/**
 * macOS text-editing keys, as a Mac terminal maps them for a shell prompt:
 * ⌘← / ⌘→ to the start and end of the line (^A / ^E), ⌘⌫ to delete back to
 * the start of it (^U), and ⌥← / ⌥→ by word (ESC b / ESC f). xterm sends
 * nothing at all for the Command ones, and `ESC[1;3D` for the Option ones,
 * which a default readline or zle does not bind.
 *
 * Only on the normal screen: a full-screen program (vim, less, htop) runs on
 * the alternate buffer and is owed the raw keys. Null means "not ours".
 */
export function macLineEditing(
  event: { key: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey: boolean; isComposing?: boolean; keyCode?: number },
  os: string | undefined,
  alternateScreen: boolean,
): string | null {
  if (os !== "macos" || alternateScreen || isImeComposition(event) || event.ctrlKey || event.shiftKey) return null;
  if (event.metaKey && !event.altKey) {
    if (event.key === "ArrowLeft") return "\x01";
    if (event.key === "ArrowRight") return "\x05";
    if (event.key === "Backspace") return "\x15";
    return null;
  }
  if (event.altKey && !event.metaKey) {
    if (event.key === "ArrowLeft") return "\x1bb";
    if (event.key === "ArrowRight") return "\x1bf";
  }
  return null;
}

/** Rows and columns a fresh PTY may be opened at: whole, finite, at least 2. */
export function spawnGridSize(dims: { rows?: number; cols?: number } | null | undefined): { rows: number; cols: number } {
  const whole = (value: number | undefined, fallback: number) =>
    typeof value === "number" && Number.isFinite(value) ? Math.min(1000, Math.max(2, Math.floor(value))) : fallback;
  return { rows: whole(dims?.rows, 24), cols: whole(dims?.cols, 80) };
}

/** The addon caps highlighted results; a capped count is never a total. */
export function terminalSearchSummary(index: number, count: number): string {
  if (count === 0) return "No matches";
  if (count >= SEARCH_HIGHLIGHT_LIMIT) return `${SEARCH_HIGHLIGHT_LIMIT}+ matches`;
  if (index < 0) return `${count} matches`;
  return `${index + 1} of ${count}`;
}
