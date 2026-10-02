/**
 * Which keystrokes belong to the shell rather than to the app.
 *
 * A terminal is a program that reads Control chords: Ctrl+K kills to the end
 * of the line in every readline and zle, Ctrl+R searches history, Ctrl+U
 * kills the line. An app-wide listener that claims one of them in the capture
 * phase takes it from the shell for as long as the app runs — the palette's
 * Ctrl+K did exactly that once it had been opened once.
 *
 * Command (macOS) chords stay the app's; a shell never sees them anyway.
 */

/** Duck-typed: `instanceof Node` throws where tests run without a DOM. */
function closestOf(target: unknown): ((selector: string) => unknown) | null {
  if (!target || typeof target !== "object") return null;
  const closest = (target as { closest?: unknown }).closest;
  return typeof closest === "function" ? (closest as (selector: string) => unknown).bind(target) : null;
}

/** The event is aimed at a live terminal's input (xterm's helper textarea or the grid). */
export function isTerminalKeyTarget(target: unknown): boolean {
  const closest = closestOf(target);
  if (!closest) return false;
  return closest("[data-terminal-session] .xterm") !== null;
}

/** A plain Control chord typed into a terminal: the shell's, not the app's. */
export function shellOwnsKey(event: {
  target?: unknown;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey?: boolean;
}): boolean {
  return event.ctrlKey && !event.metaKey && isTerminalKeyTarget(event.target);
}

/**
 * Focus is in a text field outside `host`: a rename box, a search field.
 * Pulling it into a terminal there would split the word being typed.
 */
export function isEditingElsewhere(active: unknown, host: unknown): boolean {
  if (!active || typeof active !== "object") return false;
  const element = active as { tagName?: unknown; isContentEditable?: unknown };
  const tag = typeof element.tagName === "string" ? element.tagName.toUpperCase() : "";
  const editable = tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || element.isContentEditable === true;
  if (!editable) return false;
  const contains = host && typeof host === "object" ? (host as { contains?: unknown }).contains : undefined;
  return !(typeof contains === "function" && (contains as (node: unknown) => boolean).call(host, active));
}
