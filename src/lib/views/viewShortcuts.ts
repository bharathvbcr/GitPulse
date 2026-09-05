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
 */

/** Views the native menu binds to ⌘1…⌘n, in order. */
export const VIEW_DIGIT_ORDER: readonly ViewTab[] = ["code", "history", "insights"];

/** Chord for a view, or null when it has no accelerator of its own. */
export function viewAccelerator(view: ViewTab): string | null {
  if (view === "work") return "F10";
  const index = VIEW_DIGIT_ORDER.indexOf(view);
  return index === -1 ? null : `⌘${index + 1}`;
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
 * Ctrl is excluded because Ctrl+Alt+digit already switches REPOSITORY tabs;
 * letting a section chord match it too would give one keystroke two owners,
 * which is the defect this file exists to stop repeating.
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
export function viewShortcutRows(): ShortcutRow[] {
  return REGISTERED_VIEWS.flatMap((view) => {
    const accelerator = viewAccelerator(view.id);
    if (!accelerator) return [];
    const keys = accelerator.startsWith("⌘") ? ["⌘", accelerator.slice(1)] : [accelerator];
    return [{ keys, description: `Open ${view.label}` }];
  });
}

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
