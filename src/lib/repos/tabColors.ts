/**
 * Closed palette for repository tabs and tab groups.
 *
 * Names, not CSS. A saved workspace is untrusted input, and a free-form color
 * would be an injection into style attributes. Anything outside this list is
 * refused. Group colors are stored as an array of pairs, never as an object
 * keyed by the group name, so a group called `__proto__` cannot assign a
 * prototype.
 */

export const TAB_COLORS = [
  "red",
  "orange",
  "amber",
  "green",
  "teal",
  "blue",
  "violet",
  "pink",
] as const;

export type TabColor = (typeof TAB_COLORS)[number];

const TAB_COLOR_IDS: ReadonlySet<string> = new Set(TAB_COLORS);

export const TAB_COLOR_LABEL: Record<TabColor, string> = {
  red: "Red",
  orange: "Orange",
  amber: "Amber",
  green: "Green",
  teal: "Teal",
  blue: "Blue",
  violet: "Violet",
  pink: "Pink",
};

/** Solid ink for marks and swatches. Saturated enough to read on light and dark surfaces. */
export const TAB_COLOR_INK: Record<TabColor, string> = {
  red: "#e11d48",
  orange: "#ea580c",
  amber: "#d97706",
  green: "#059669",
  teal: "#0d9488",
  blue: "#2563eb",
  violet: "#7c3aed",
  pink: "#db2777",
};

export interface GroupColor {
  readonly group: string;
  readonly color: TabColor;
}

export function normalizeTabColor(raw: unknown): TabColor | null {
  if (typeof raw !== "string") return null;
  const id = raw.normalize("NFC").trim().toLowerCase();
  return TAB_COLOR_IDS.has(id) ? (id as TabColor) : null;
}

/**
 * `group` must already be a normalized group name. Comparison is exact so a
 * caller that forgot to normalize does not silently miss.
 */
export function lookupGroupColor(
  colors: readonly GroupColor[] | null | undefined,
  group: string | null | undefined,
): TabColor | null {
  if (!group || !colors) return null;
  for (const entry of colors) {
    if (entry?.group === group) return normalizeTabColor(entry.color);
  }
  return null;
}

/** A tab's own color wins. Otherwise the group color shows through. */
export function effectiveTabColor(own: unknown, groupColor: unknown): TabColor | null {
  return normalizeTabColor(own) ?? normalizeTabColor(groupColor);
}
