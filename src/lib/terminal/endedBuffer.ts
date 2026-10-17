/**
 * Scrollback an ended, off-screen terminal is allowed to keep.
 *
 * A repository strip holds up to `MAX_STRIP_TABS` tabs, ended ones included,
 * and every repository tab that has hosted a dock keeps that strip mounted.
 * Repository tabs have no ceiling. A filled shell scrollback is about 12 MB
 * of cells (`cols * 12` per row), so finished sessions would retain that for
 * as long as their tabs stayed open, once per tab and once per repository.
 *
 * A live session is left alone: it is still taking output, and hiding it is
 * what keeps its scrollback across a switch. An ended session nobody is
 * looking at drops its scrollback. xterm keeps the last screen in the
 * viewport, so that screen stays. A short plain-text tail is remembered for
 * a viewport that is empty (a recreated terminal), and it is not written
 * onto a screen that already shows the end. The tail store is
 * process-global, so the bound does not grow with repository tabs.
 */

/** Characters of an ended transcript kept after its scrollback is released. */
export const ENDED_TAIL_CHARS = 32_768;
/** Ended transcripts remembered at once, across every repository. */
export const MAX_ENDED_TAILS = 32;

const tails = new Map<string, string>();

/** True when this session's scrollback should be dropped. */
export function shouldReleaseEndedBuffer(status: string | undefined, onScreen: boolean): boolean {
  return status === "exited" && !onScreen;
}

/** The end of `text`, bounded by `max` characters. */
export function clipEndedTail(text: string, max = ENDED_TAIL_CHARS): string {
  if (text.length <= max) return text;
  return text.slice(text.length - max);
}

/**
 * Remembers the tail, dropping the oldest when the global cap is passed.
 * Empty text does not forget a tail already stored for `id`.
 */
export function rememberEndedTail(id: string, text: string): void {
  const clipped = clipEndedTail(text);
  if (!clipped) return;
  tails.delete(id);
  tails.set(id, clipped);
  while (tails.size > MAX_ENDED_TAILS) {
    const oldest = tails.keys().next().value;
    if (oldest === undefined) break;
    tails.delete(oldest);
  }
}

/** The tail last remembered for `id`, or undefined when none was kept. */
export function endedTail(id: string): string | undefined {
  const text = tails.get(id);
  if (text === undefined) return undefined;
  tails.delete(id);
  tails.set(id, text);
  return text;
}

/** Drops a tail when its tab is closed. */
export function forgetEndedTail(id: string): void {
  tails.delete(id);
}

/** How many tails are retained. For tests. */
export function endedTailCount(): number {
  return tails.size;
}

/** Clears the store. For tests. */
export function resetEndedTails(): void {
  tails.clear();
}

/** The bit of an xterm this release touches: its scrollback option. */
export interface ScrollbackTerminal {
  options: { scrollback?: number };
}

/**
 * Drops retained rows, then puts the configured `limit` back. xterm trims the
 * moment scrollback is lowered, and raising it afterwards does not allocate
 * the rows back until new output arrives — which an ended session does not
 * produce.
 *
 * The limit goes back at once because a terminal left at 0 has no room above
 * its screen. A hidden dock is still fitted (to a sliver), and xterm's
 * narrowing reflow at 0 leaves `ybase` past the buffer; growing back then
 * scrolls into that phantom row instead of adding one, so the screen has
 * more rows than the buffer has lines and the WebGL renderer reads a line
 * that is not there.
 */
export function releaseEndedScrollback(term: ScrollbackTerminal, limit: number): void {
  term.options.scrollback = 0;
  term.options.scrollback = limit;
}
