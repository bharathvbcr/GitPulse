/**
 * Whether an element's focus is keyboard-driven (`:focus-visible`).
 *
 * Owns the modality check for focus-driven tooltip cards so its failure
 * policy lives in exactly one tested place. It fails OPEN: an environment
 * where the selector is unsupported (older engines, headless DOMs) treats
 * focus as keyboard, so the accessible affordance appears rather than
 * silently never showing. A pointer user in such an environment sees one
 * extra card; a keyboard user in a normal browser never loses theirs.
 */
export function isKeyboardFocus(element: Element): boolean {
  try {
    return element.matches(":focus-visible");
  } catch {
    return true;
  }
}
