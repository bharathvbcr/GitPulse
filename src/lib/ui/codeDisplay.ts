/**
 * How code is drawn wherever GitPulse renders it — the diff, the file viewer,
 * blame and the conflict editor.
 *
 * These are preference *defaults*, not live state. The diff toolbar still
 * flips layout, wrap and syntax for the file in front of you; what changes
 * here is what those start as when a diff opens, which is the part that used
 * to be unconfigurable and reset on every mount.
 */

/** Unified is one column with +/- rows; split is old beside new. */
export type DiffLayout = "unified" | "split";

export const DIFF_LAYOUTS: readonly DiffLayout[] = ["unified", "split"];

export function isDiffLayout(value: unknown): value is DiffLayout {
  return value === "unified" || value === "split";
}

/**
 * Columns a literal tab advances to.
 *
 * 8 is the CSS initial value, so it is also the default here: the setting is
 * additive, and a fresh install renders exactly what it rendered before.
 */
export const TAB_WIDTHS = [2, 4, 8] as const;

export type TabWidth = (typeof TAB_WIDTHS)[number];

export const DEFAULT_TAB_WIDTH: TabWidth = 8;

export function isTabWidth(value: unknown): value is TabWidth {
  return (TAB_WIDTHS as readonly number[]).includes(value as number);
}

/** The element carrying the inherited `tab-size`; normally `<html>`. */
export interface TabWidthTarget {
  readonly style: { setProperty(name: string, value: string): void };
}

/**
 * Publishes the tab width as a custom property on the document root.
 *
 * `tab-size` inherits, and app.css sets it once on `html` from this variable,
 * so every `white-space: pre` surface follows without each viewer needing to
 * know the preference exists. A value outside the offered set falls back
 * rather than writing an arbitrary number into the stylesheet.
 */
export function applyTabWidth(width: number, target?: TabWidthTarget | null): void {
  const root =
    target ?? (typeof document !== "undefined" ? document.documentElement : null);
  if (!root) return;
  root.style.setProperty(
    "--gp-tab-size",
    String(isTabWidth(width) ? width : DEFAULT_TAB_WIDTH),
  );
}
