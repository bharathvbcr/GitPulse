/**
 * What the view bar's search field says when it is the command palette, and
 * how much of it, for the width the field has.
 *
 * The field's floor is deliberately narrow (96px below a 1400px row: at the
 * default window Work's switcher needs the rest), so a fixed sentence used to
 * be cut mid-word ("Sea ⌘F"). The copy now degrades in whole phrases instead:
 * the long hint, then the subject alone, then the word "Search", and the
 * shortcut hint goes before any text is cut for it. The text is picked from
 * the field's measured width (a ResizeObserver in PageSearch), not the window's.
 */

/** Content-box width the icon, the input's own padding and the gaps take. */
export const FIELD_CHROME_PX = 32;
/** What the ⌘F hint takes, margin included. */
export const FIELD_KEYCAP_PX = 40;
/** A deliberately generous glyph width for 12px text, so a pick never overflows. */
export const FIELD_CHAR_PX = 6.6;
/** Below this field width the shortcut hint is dropped so the text keeps the room. */
export const FIELD_KEYCAP_MIN_WIDTH = 170;

export interface PaletteFieldInput {
  /**
   * What is searched: "this repository" on the repository surface, the
   * repository's display name on Fleet, Agents and Tasks, or null when no
   * repository is open.
   */
  subject: string | null;
  /** The field's content-box width in px, or null when it has not been measured. */
  width: number | null;
}

export interface PaletteFieldCopy {
  placeholder: string;
  /** The accessible name. It does not change with width. */
  label: string;
  showKeycap: boolean;
}

export function paletteFieldCopy({ subject, width }: PaletteFieldInput): PaletteFieldCopy {
  const label = subject ? `Search ${subject}` : "Search commands";
  const long = subject
    ? `Search ${subject}: commands, / files, @ branches, # commits`
    : "Search commands, % repositories";
  const showKeycap = width === null || width >= FIELD_KEYCAP_MIN_WIDTH;
  if (width === null) return { placeholder: long, label, showKeycap };
  const room = width - FIELD_CHROME_PX - (showKeycap ? FIELD_KEYCAP_PX : 0);
  const placeholder = [long, label].find((text) => text.length * FIELD_CHAR_PX <= room) ?? "Search";
  return { placeholder, label, showKeycap };
}
