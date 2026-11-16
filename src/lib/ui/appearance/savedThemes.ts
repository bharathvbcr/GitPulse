/**
 * The reader's own theme library ("My themes" in the gallery).
 *
 * `customTheme` stays the one applied slot; this is the shelf beside it, so a
 * reader can keep several imported or saved themes and switch between them.
 * Every entry is untrusted on read, exactly like `customTheme`: it goes back
 * through `validateTheme`, and anything that fails is dropped rather than
 * repaired. The list is bounded so it cannot grow the preference blob, which is
 * saved whole on every change, without limit.
 */
import { validateTheme, type GitPulseTheme } from "./themeSchema";

/** Most themes kept. A theme is a few hundred bytes; this bounds the blob at a few KB. */
export const MAX_SAVED_THEMES = 24;

/** Reads a stored list: validated, de-duplicated by name and appearance, bounded. */
export function readSavedThemes(raw: unknown): readonly GitPulseTheme[] {
  if (!Array.isArray(raw)) return [];
  const out: GitPulseTheme[] = [];
  const seen = new Set<string>();
  for (const item of raw.slice(0, MAX_SAVED_THEMES * 2)) {
    const checked = validateTheme(item);
    if (!checked.ok) continue;
    const key = savedThemeKey(checked.theme);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(checked.theme);
    if (out.length === MAX_SAVED_THEMES) break;
  }
  return out;
}

/** Identity on the shelf: the same name in the same appearance is the same theme. */
export function savedThemeKey(theme: Pick<GitPulseTheme, "name" | "appearance">): string {
  return `${theme.appearance}\u0000${theme.name.toLowerCase()}`;
}

export type SaveOutcome = "added" | "replaced" | "full";

/**
 * `list` with `theme` on it: a theme with the same name and appearance is
 * replaced in place, a new one goes to the front, and a full shelf refuses
 * rather than silently evicting someone's theme.
 */
export function withSavedTheme(
  list: readonly GitPulseTheme[],
  theme: GitPulseTheme,
): { readonly list: readonly GitPulseTheme[]; readonly outcome: SaveOutcome } {
  const key = savedThemeKey(theme);
  const at = list.findIndex((entry) => savedThemeKey(entry) === key);
  if (at >= 0) {
    const next = [...list];
    next[at] = theme;
    return { list: next, outcome: "replaced" };
  }
  if (list.length >= MAX_SAVED_THEMES) return { list, outcome: "full" };
  return { list: [theme, ...list], outcome: "added" };
}

export function withoutSavedTheme(
  list: readonly GitPulseTheme[],
  theme: Pick<GitPulseTheme, "name" | "appearance">,
): readonly GitPulseTheme[] {
  const key = savedThemeKey(theme);
  return list.filter((entry) => savedThemeKey(entry) !== key);
}
