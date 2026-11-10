/**
 * Canonical z-index tiers for every overlay surface in the app.
 *
 * Ordering intent, lowest → highest:
 *
 *   STICKY (10) — a header or row that sticks over a scroller. It sits above
 *     the scroller's content and below everything that floats.
 *   SCROLL_CUE (20) — the chevron overlay at a scroller's edge
 *     (`gp-scroll-cue` in app.css pins the same number). Above a sticky header
 *     it shares the scroller with, below every overlay.
 *   DROP_OVERLAY (40) — the full-window "drop a repo here" veil sits under
 *     everything so ordinary UI stays readable through it.
 *   MENU / MODAL (50) — context menus and standard dialogs share a tier;
 *     they are mutually exclusive surfaces (opening one closes the other),
 *     so ties between them never paint at once.
 *   PROMPT (60) — the modal prompt (branch rename, confirms) intentionally
 *     stacks ABOVE other dialogs: it is opened from within them.
 *   TOAST (65) — notifications stay readable over a dialog and its prompt
 *     (an error raised from inside a confirm must not hide behind it), but
 *     below the tooltip layer.
 *   TOOLTIP (70) — always readable, even over the prompt.
 *
 * Consume via inline `style="z-index: {LAYERS.X}"` rather than Tailwind
 * arbitrary-value classes: interpolated class names would not be seen by
 * the Tailwind scanner.
 */
export const LAYERS = {
  STICKY: 10,
  SCROLL_CUE: 20,
  DROP_OVERLAY: 40,
  TOUR: 45, // Live guide stays below menus, trust prompts, and Settings.
  MENU: 50,
  MODAL: 50,
  PROMPT: 60,
  TOAST: 65,
  TOOLTIP: 70,
} as const;
