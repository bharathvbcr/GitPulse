/**
 * The canvas adapter's boundary: resolve the app's `--c-*` colour tokens
 * (bare `R G B` triplets) into the renderer's theme. The renderer itself knows
 * no tokens, so it can move into a standalone package unchanged.
 */

import { DEFAULT_CODE_GRAPH_THEME, type CodeGraphTheme } from "./CodeGraphRenderer";

/** Reads one custom property, e.g. `--c-text`; returns "" when it is unset. */
export type TokenReader = (name: string) => string;

const TRIPLET = /^\s*(\d{1,3})[\s,]+(\d{1,3})[\s,]+(\d{1,3})\s*$/;

/** `20 26 41` + 0.5 becomes `rgb(20 26 41 / 0.5)`; anything else is unusable. */
function tone(read: TokenReader, token: string, alpha: number, fallback: string): string {
  const match = TRIPLET.exec(read(token));
  if (!match) return fallback;
  const [, r, g, b] = match;
  return alpha >= 1 ? `rgb(${r} ${g} ${b})` : `rgb(${r} ${g} ${b} / ${alpha})`;
}

export function codeGraphThemeFromTokens(read: TokenReader): CodeGraphTheme {
  const d = DEFAULT_CODE_GRAPH_THEME;
  return {
    background: null,
    link: tone(read, "--c-text-muted", 0.5, d.link),
    linkMuted: tone(read, "--c-text-muted", 0.45, d.linkMuted),
    label: tone(read, "--c-text", 0.9, d.label),
    selection: tone(read, "--c-text", 1, d.selection),
    hover: tone(read, "--c-accent", 1, d.hover),
    plate: tone(read, "--c-surface", 1, d.plate),
    field: tone(read, "--c-text-muted", 1, d.field),
    dead: tone(read, "--c-status-red", 1, d.dead),
    unwired: tone(read, "--c-status-amber", 1, d.unwired),
    entry: tone(read, "--c-status-emerald", 1, d.entry),
  };
}

/** Resolve the theme from the computed style of `element` (and what it inherits). */
export function codeGraphThemeFromElement(element: Element): CodeGraphTheme {
  const style = getComputedStyle(element);
  return codeGraphThemeFromTokens((name) => style.getPropertyValue(name));
}
