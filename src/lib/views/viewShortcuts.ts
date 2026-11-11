import type { HostOS } from "../platform";
import type { ViewTab } from "../repos/persist";
import { REGISTERED_VIEWS, sectionsFor, type ViewSection } from "./viewRegistry";

/**
 * The keyboard map for views and their sections.
 *
 * One source of truth, because the cheat sheet drifted from reality the moment
 * the app consolidated: it still promised "⌘1–9 — Files, Graph, Diff, Resolve,
 * Blame, Stack, GitHub, Coverage, Health" long after those nine views became
 * four. A hand-written list of chords is a list that will be wrong again, so
 * the sheet is derived from here and a contract test holds the native menu to
 * the same table.
 *
 * Digits are assigned in registry (= header) order. Work is deliberately
 * absent: it is the default view and the native menu gives it F10, which is
 * recorded here rather than in prose so the sheet cannot disagree.
 *
 * The plain command digits, ⌘1–⌘9 (Ctrl+1–9 elsewhere), select REPOSITORY
 * tabs, as in a browser. Views therefore take a chord of their own, and it is
 * not the same word on every host:
 *   - macOS: ⌥⌘1–3. Never ⌘⇧3/4/5, which are the system screenshot shortcuts.
 *   - Windows and Linux: Ctrl+Shift+1–3. Ctrl+Alt+digit is AltGr on many
 *     keyboard layouts, so it types a character instead of reaching the menu,
 *     and plain Alt+digit is the section chord below.
 * `viewChordModifiers` is the structured form; `menu.rs` binds the same chords
 * and `viewShortcuts.test.ts` holds the two to each other.
 */

/** Views the native menu binds to the view chord's digits 1…n, in order. */
export const VIEW_DIGIT_ORDER: readonly ViewTab[] = ["code", "history", "insights"];

export type ChordModifier = "ctrl" | "alt" | "shift" | "meta";

/** The modifiers of a view chord on `os`, in a fixed order. */
export function viewChordModifiers(os: HostOS): readonly ChordModifier[] {
  return os === "macos" ? ["alt", "meta"] : ["ctrl", "shift"];
}

/** The modifiers of a repository-tab chord on `os`: the host's command key alone. */
export function repoTabChordModifiers(os: HostOS): readonly ChordModifier[] {
  return os === "macos" ? ["meta"] : ["ctrl"];
}

/**
 * Chord for a view, or null when it has no accelerator of its own. Authored in
 * macOS notation on macOS and spelled out elsewhere, because the Windows and
 * Linux chord is not a mechanical translation of the macOS one.
 */
export function viewAccelerator(view: ViewTab, os: HostOS = "macos"): string | null {
  if (view === "work") return "F10";
  const index = VIEW_DIGIT_ORDER.indexOf(view);
  if (index === -1) return null;
  return os === "macos" ? `⌥⌘${index + 1}` : `Ctrl+Shift+${index + 1}`;
}

/**
 * Sections are reached with ⌥ plus their position in the active view.
 *
 * The consolidation turned fifteen destinations into four views and fourteen
 * sections, and only the views kept accelerators — so Diff, Blame, Coverage
 * and Policy went from one chord to a click or a palette phrase. ⌥ is used
 * rather than more ⌘ digits because the section is scoped to whichever view is
 * open: the same chord means "the second lens of what I am looking at".
 */
export const SECTION_MODIFIER = "⌥";

/** Chord for the `index`-th section of a view (0-based). */
export function sectionAccelerator(index: number): string {
  return `${SECTION_MODIFIER}${index + 1}`;
}

/**
 * `KeyboardEvent.code` values that select a section, in order.
 *
 * Matched on `code`, not `key`: ⌥1 on a Mac produces "¡", so a `key` test
 * silently never fires — the failure mode of a shortcut that works on one
 * keyboard layout and not another.
 */
export const SECTION_KEY_CODES: readonly string[] = [
  "Digit1",
  "Digit2",
  "Digit3",
  "Digit4",
  "Digit5",
  "Digit6",
  "Digit7",
  "Digit8",
  "Digit9",
];

export interface SectionChord {
  /** Which modifier combination counts as a section chord. */
  alt: boolean;
  ctrl: boolean;
  meta: boolean;
  shift: boolean;
  code: string;
}

/**
 * The section a chord selects within `view`, or null when it selects none.
 *
 * Returns null rather than clamping for an out-of-range digit: ⌥5 in a view
 * with three sections must fall through to whatever else wants it, not quietly
 * land on the last one.
 *
 * Ctrl, Meta and Shift are excluded because every other digit chord has
 * another owner: ⌘/Ctrl+digit select repository tabs, and ⌥⌘digit (macOS) or
 * Ctrl+Shift+digit (elsewhere) select views. Letting a section chord match one
 * of them too would give one keystroke two owners, which is the defect this
 * file exists to stop repeating.
 */
export function sectionForChord(view: ViewTab, chord: SectionChord): ViewSection | null {
  if (!chord.alt || chord.ctrl || chord.meta || chord.shift) return null;
  const index = SECTION_KEY_CODES.indexOf(chord.code);
  if (index === -1) return null;
  const sections = sectionsFor(view);
  return sections[index] ?? null;
}

export interface ShortcutRow {
  keys: string[];
  description: string;
}

/** Cheat-sheet rows for every view accelerator, derived from the registry. */
export function viewShortcutRows(os: HostOS = "macos"): ShortcutRow[] {
  return REGISTERED_VIEWS.flatMap((view) => {
    const accelerator = viewAccelerator(view.id, os);
    if (!accelerator) return [];
    const index = VIEW_DIGIT_ORDER.indexOf(view.id);
    // One chip per key. A digit chord is its modifiers, then the digit.
    const keys =
      index === -1
        ? [accelerator]
        : [
            ...viewChordModifiers(os).map((modifier) => MODIFIER_KEYS[modifier]),
            String(index + 1),
          ];
    return [{ keys, description: `Open ${view.label}` }];
  });
}

/** Key labels in macOS notation; `shortcutKeyLabels` maps them for other hosts. */
const MODIFIER_KEYS: Readonly<Record<ChordModifier, string>> = {
  ctrl: "⌃",
  alt: "⌥",
  shift: "⇧",
  meta: "⌘",
};

/** Cheat-sheet rows for the sections of every view that has more than one. */
export function sectionShortcutRows(): ShortcutRow[] {
  return REGISTERED_VIEWS.flatMap((view) => {
    const sections = sectionsFor(view.id);
    if (sections.length < 2) return [];
    const range =
      sections.length === 1
        ? "1"
        : `1–${Math.min(sections.length, SECTION_KEY_CODES.length)}`;
    return [
      {
        keys: [SECTION_MODIFIER, range],
        description: `${view.label} sections: ${sections.map((s) => s.label).join(", ")}`,
      },
    ];
  });
}
