/**
 * Theme and motion preferences for the page shell.
 *
 * The renderer takes an injected `CodeGraphTheme`; this page resolves it from
 * the CSS custom properties in `styles.ts`, so one declaration of each colour
 * (light and dark) drives both the DOM and the canvas.
 */

import { DEFAULT_CODE_GRAPH_THEME, type CodeGraphTheme } from "../codeGraphRenderer";

function token(style: CSSStyleDeclaration, name: string, fallback: string): string {
  const value = style.getPropertyValue(name).trim();
  return value || fallback;
}

/** Resolve the canvas theme from the custom properties in scope at `el`. */
export function themeFromCss(el: Element): CodeGraphTheme {
  const style = getComputedStyle(el);
  const base = DEFAULT_CODE_GRAPH_THEME;
  return {
    background: token(style, "--cm-canvas", "") || null,
    link: token(style, "--cm-link", base.link),
    linkMuted: token(style, "--cm-link-muted", base.linkMuted),
    label: token(style, "--cm-label", base.label),
    selection: token(style, "--cm-accent", base.selection),
    hover: token(style, "--cm-hover", base.hover),
    plate: token(style, "--cm-plate", base.plate),
    field: token(style, "--cm-field", base.field),
    dead: token(style, "--cm-dead", base.dead),
    unwired: token(style, "--cm-unwired", base.unwired),
    entry: token(style, "--cm-entry", base.entry),
  };
}

export function prefersLight(): boolean {
  return typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: light)").matches;
}

export function prefersReducedMotion(): boolean {
  return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function easeOutCubic(t: number): number {
  return 1 - (1 - t) ** 3;
}

/** Call `listener` whenever the colour scheme flips. Returns the unsubscribe. */
export function onSchemeChange(listener: () => void): () => void {
  if (typeof matchMedia !== "function") return () => undefined;
  const query = matchMedia("(prefers-color-scheme: light)");
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}
