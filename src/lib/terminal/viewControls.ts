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

/**
 * Rows of history one terminal keeps.
 *
 * xterm holds a scrollback row as `cols * 12` bytes whether or not it has
 * text in it, so a filled 5,000-row session at 200 columns holds 11.6 MB
 * (measured against the bundled xterm 6.0.0 with `node --expose-gc`; the
 * numbers are in the CHANGELOG entry that introduced this setting). Every
 * visited repository tab keeps its terminals mounted, so the figure is paid
 * once per session, not once per window.
 *
 * Two settings, because the two kinds of session want different things: an
 * interactive shell is where someone scrolls back to what a command printed,
 * while an agent CLI redraws its own interface and its useful history is the
 * transcript it keeps itself.
 */
export const TERMINAL_SCROLLBACK_MIN = 100;
export const TERMINAL_SCROLLBACK_MAX = 50_000;
export const TERMINAL_SCROLLBACK_DEFAULT = 5_000;
export const TERMINAL_AGENT_SCROLLBACK_DEFAULT = 1_000;

/** Whole rows within the bounds; anything unreadable takes `fallback`. */
export function clampTerminalScrollback(value: unknown, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return fallback;
  return Math.max(TERMINAL_SCROLLBACK_MIN, Math.min(TERMINAL_SCROLLBACK_MAX, Math.round(value)));
}

/**
 * The scrollback a session gets: an agent CLI (anything but a plain shell) or
 * a task run takes the smaller setting.
 */
export function terminalScrollbackFor(
  session: { launcher: string; taskRunId?: string },
  settings: { terminalScrollback: number; terminalAgentScrollback: number },
): number {
  const agent = session.launcher !== "shell" || !!session.taskRunId;
  return clampTerminalScrollback(
    agent ? settings.terminalAgentScrollback : settings.terminalScrollback,
    agent ? TERMINAL_AGENT_SCROLLBACK_DEFAULT : TERMINAL_SCROLLBACK_DEFAULT,
  );
}

/**
 * xterm's Option handling. By default macOS composes Option+key into a
 * glyph, so Option+B types "∫" instead of sending ESC b and shells and
 * agent CLIs lose word-wise motion and Alt shortcuts. Treating Option as
 * Meta sends ESC-prefixed keys; Option+click is the escape hatch that still
 * selects text when a program has mouse reporting on. Off macOS the option
 * does nothing and is left unset.
 *
 * `asMeta` is the user's setting (default on): the cost of Meta is that a
 * layout which types a character with Option, such as `@` on a German
 * keyboard, cannot type it in a terminal, so those users turn it off.
 */
export function macOptionOptions(
  os: string | undefined,
  asMeta = true,
): {
  macOptionIsMeta: boolean;
  macOptionClickForcesSelection: boolean;
} {
  const on = os === "macos" && asMeta;
  return { macOptionIsMeta: on, macOptionClickForcesSelection: on };
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

/**
 * Rows and columns a fresh PTY may be opened at: whole, finite, at least 2.
 *
 * `rendered` is whether the terminal's element has a box at all. A terminal
 * started out of sight — a task agent launched from its task sheet, into a
 * dock nobody has opened — is measured inside `display: none`, and xterm's
 * proposal there is not NaN but a confident small grid (6 × 11 was seen). An
 * agent told it has 11 columns lays out its whole first screen for them, and
 * that output stays wrapped in the scrollback after the dock is opened and
 * the grid corrected. So an unrendered measurement is not used: the PTY
 * starts at the conventional 24 × 80 and is refitted when it is first shown.
 */
export function spawnGridSize(
  dims: { rows?: number; cols?: number } | null | undefined,
  rendered = true,
): { rows: number; cols: number } {
  const whole = (value: number | undefined, fallback: number) =>
    typeof value === "number" && Number.isFinite(value) ? Math.min(1000, Math.max(2, Math.floor(value))) : fallback;
  if (!rendered) return { rows: 24, cols: 80 };
  return { rows: whole(dims?.rows, 24), cols: whole(dims?.cols, 80) };
}

/** Whether an element is laid out with a box (not inside `display: none`). */
export function hasRenderedBox(element: { getClientRects(): { length: number }; clientWidth: number; clientHeight: number } | null | undefined): boolean {
  return !!element && element.getClientRects().length > 0 && element.clientWidth > 0 && element.clientHeight > 0;
}

/** The addon caps highlighted results; a capped count is never a total. */
export function terminalSearchSummary(index: number, count: number): string {
  if (count === 0) return "No matches";
  if (count >= SEARCH_HIGHLIGHT_LIMIT) return `${SEARCH_HIGHLIGHT_LIMIT}+ matches`;
  if (index < 0) return `${count} matches`;
  return `${index + 1} of ${count}`;
}
