/**
 * True when the keystroke belongs to an IME composition session rather than
 * plain key input. During Japanese/Chinese/Korean composition the browser
 * reports Enter/Arrow keys with `isComposing` set (and legacy engines use
 * keyCode 229); treating those as commands would execute actions while the
 * user is only confirming a conversion.
 */
export function isImeComposition(e: { isComposing?: boolean; keyCode?: number }): boolean {
  return e.isComposing === true || e.keyCode === 229;
}
