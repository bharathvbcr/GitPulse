/**
 * The accent palette the user can pick from.
 *
 * Each accent carries one RGB triplet per theme rather than a single colour,
 * because the two themes need opposite ends of the same hue: accent text sits
 * on `--c-surface-hover`, which is near-black in dark and near-white in light,
 * and one triplet cannot be legible on both. Every pair here is checked
 * against WCAG AA by `accents.contract.test.ts`, which derives its cases from
 * this list — adding an accent adds its contrast test rather than needing one
 * written by hand.
 *
 * `blue` is the shipped default and its triplets are exactly the `--c-accent`
 * values declared in app.css, so choosing it means "use the stylesheet" and
 * the applier removes the override entirely.
 */

export type Theme = "dark" | "light";

/**
 * Declared as a tuple so `AccentId` is a literal union rather than `string`.
 * Derived from `ACCENTS` it would widen to `string`, and `setAccent("teel")`
 * would typecheck and persist a colour that does not exist. Same shape as
 * SETTINGS_SECTION_IDS, and the catalog below is checked against it.
 */
export const ACCENT_IDS = ["blue", "violet", "teal", "green", "amber", "rose"] as const;

export type AccentId = (typeof ACCENT_IDS)[number];

export interface Accent {
  readonly id: AccentId;
  /** Rail label and accessible name. */
  readonly label: string;
  /** `R G B` channels per theme, in the space `--c-accent` is declared in. */
  readonly rgb: Readonly<Record<Theme, readonly [number, number, number]>>;
}

export const ACCENTS: readonly Accent[] = [
  {
    id: "blue",
    label: "Blue",
    rgb: { dark: [128, 158, 255], light: [48, 64, 175] },
  },
  {
    id: "violet",
    label: "Violet",
    rgb: { dark: [189, 136, 242], light: [112, 22, 202] },
  },
  {
    id: "teal",
    label: "Teal",
    rgb: { dark: [25, 176, 200], light: [12, 88, 100] },
  },
  {
    id: "green",
    label: "Green",
    rgb: { dark: [43, 182, 108], light: [21, 91, 54] },
  },
  {
    id: "amber",
    label: "Amber",
    rgb: { dark: [222, 140, 18], light: [113, 72, 9] },
  },
  {
    id: "rose",
    label: "Rose",
    rgb: { dark: [234, 128, 142], light: [158, 26, 43] },
  },
];

/** The accent applied when nothing is stored, or when a stored id is unknown. */
export const DEFAULT_ACCENT: AccentId = "blue";

export function isAccentId(value: unknown): value is AccentId {
  return (ACCENT_IDS as readonly string[]).includes(value as string);
}

export function accentFor(id: string): Accent {
  return ACCENTS.find((accent) => accent.id === id) ?? ACCENTS[0];
}

/** `"128 158 255"` — the channel form `rgb(var(--c-accent) / …)` expects. */
export function accentChannels(id: string, theme: Theme): string {
  return accentFor(id).rgb[theme].join(" ");
}

/** CSS-only swatch fill, for the picker itself. */
export function accentSwatch(id: string, theme: Theme): string {
  return `rgb(${accentFor(id).rgb[theme].join(" ")})`;
}

/** The element whose inline style carries the override; normally `<html>`. */
export interface AccentTarget {
  readonly style: {
    setProperty(name: string, value: string): void;
    removeProperty(name: string): void;
  };
}

/**
 * Writes the accent for `theme` as an inline custom property.
 *
 * Inline style beats the `html.dark` / `html.light` rules on the same element,
 * so this overrides the stylesheet without touching it — and the default
 * removes the property instead of re-stating it, which keeps app.css the one
 * place the shipped accent is declared (and keeps the existing contrast
 * contract over that stylesheet meaningful).
 *
 * Only `--c-accent` is written: `--shadow-glow`, `--ring-focus`, the macOS
 * glass tint and the canvas graph's `--accent-color` are all declared in terms
 * of it, so they follow from this one assignment.
 */
export function applyAccent(
  id: string,
  theme: Theme,
  target?: AccentTarget | null,
): void {
  const root =
    target ?? (typeof document !== "undefined" ? document.documentElement : null);
  if (!root) return;
  if (!isAccentId(id) || id === DEFAULT_ACCENT) {
    root.style.removeProperty("--c-accent");
    return;
  }
  root.style.setProperty("--c-accent", accentChannels(id, theme));
}
