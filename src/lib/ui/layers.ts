/**
 * Canonical z-index tiers for every overlay surface in the app.
 *
 * Ordering intent, lowest → highest:
 *
 *   DROP_OVERLAY (40) — the full-window "drop a repo here" veil sits under
 *     everything so ordinary UI stays readable through it.
 *   MENU / MODAL (50) — context menus and standard dialogs share a tier;
 *     they are mutually exclusive surfaces (opening one closes the other),
 *     so ties between them never paint at once.
 *   PROMPT (60) — the modal prompt (branch rename, confirms) intentionally
 *     stacks ABOVE other dialogs: it is opened from within them.
 *   TOOLTIP (70) — always readable, even over the prompt.
 *
 * Consume via inline `style="z-index: {LAYERS.X}"` rather than Tailwind
 * arbitrary-value classes: interpolated class names would not be seen by
 * the Tailwind scanner.
 */
export const LAYERS = {
  DROP_OVERLAY: 40,
  MENU: 50,
  MODAL: 50,
  PROMPT: 60,
  TOOLTIP: 70,
} as const;
