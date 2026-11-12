/**
 * Whether the header's buttons carry words.
 *
 * "auto" is the default: the header's own width decides, through the container
 * query tiers in HEADER_SPEC section 4 (labels from 1100 px). "icons" is the
 * reader's override: the narrow rendering at every width, so a wide window can
 * still be an icon-only title bar.
 *
 * The override is one attribute on `<html>` and one stylesheet
 * (`headerLabels.css`) that answers it, so no header component has to know the
 * preference exists. The attribute is present only for "icons": absent means
 * "let the width decide".
 */
import type { Readable } from "svelte/store";
import "./headerLabels.css";

export const HEADER_LABEL_MODES = ["auto", "icons"] as const;
export type HeaderLabelMode = (typeof HEADER_LABEL_MODES)[number];

export const HEADER_LABELS_ATTRIBUTE = "data-header-labels";

export function isHeaderLabelMode(value: unknown): value is HeaderLabelMode {
  return typeof value === "string" && (HEADER_LABEL_MODES as readonly string[]).includes(value);
}

/**
 * Reads the preference from a stored blob.
 *
 * `showHeaderActionLabels` is the boolean this setting was stored as before the
 * header was rebuilt: `false` was a deliberate "no words", so it comes back as
 * "icons". `true` was the shipped default and so says nothing the width does not.
 * The current key wins when both are present and valid.
 */
export function readHeaderLabelMode(
  stored: { headerLabels?: unknown; showHeaderActionLabels?: unknown },
  fallback: HeaderLabelMode,
): HeaderLabelMode {
  if (isHeaderLabelMode(stored.headerLabels)) return stored.headerLabels;
  if (stored.showHeaderActionLabels === false) return "icons";
  if (stored.showHeaderActionLabels === true) return "auto";
  return fallback;
}

export interface HeaderLabelsTarget {
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
}

export function applyHeaderLabels(mode: HeaderLabelMode, target?: HeaderLabelsTarget | null): void {
  const root = target ?? (typeof document === "undefined" ? null : document.documentElement);
  if (!root) return;
  if (mode === "icons") root.setAttribute(HEADER_LABELS_ATTRIBUTE, "icons");
  else root.removeAttribute(HEADER_LABELS_ATTRIBUTE);
}

/** Keeps the attribute in step with the store; returns the unsubscribe. */
export function installHeaderLabels(
  prefs: Readable<{ readonly headerLabels: HeaderLabelMode }>,
  target?: HeaderLabelsTarget | null,
): () => void {
  return prefs.subscribe(({ headerLabels }) => applyHeaderLabels(headerLabels, target));
}
