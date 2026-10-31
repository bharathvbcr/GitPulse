/**
 * Whether a modal dialog is already on screen, so a global shortcut that would
 * open another one should stand down. Two modals at one z-index fight for
 * focus and the second is painted by DOM order, not by intent.
 *
 * `except` names an attribute on the one surface that is allowed to be open
 * (the command palette marks itself `data-command-palette`).
 *
 * Duplicate to fold: `CommandPalette.svelte` has its own `modalOccupied()`
 * with the same selector, and `App.svelte` opens the shortcuts sheet on `?`
 * without asking. Both should delegate here in the cross-cutting pass.
 */
export function isModalOccupied(
  doc: Pick<Document, "querySelector"> = document,
  except?: string,
): boolean {
  const selector = except ? `[aria-modal="true"]:not([${except}])` : '[aria-modal="true"]';
  return doc.querySelector(selector) !== null;
}
