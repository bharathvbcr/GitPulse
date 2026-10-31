import { isImeComposition } from "./imeGuard";

type KeyLike = { key: string; isComposing?: boolean; keyCode?: number };

/**
 * An Escape that should close a dialog. The Escape that confirms or cancels an
 * IME candidate (Japanese, Chinese, Korean) is not one: it belongs to the
 * composition, and closing on it discards the dialog the person was typing in.
 */
export function isDialogEscape(event: KeyLike): boolean {
  return event.key === "Escape" && !isImeComposition(event);
}

/**
 * Whether Escape in a dialog's search field should clear the field and stop
 * there. With text in it, Escape means "clear this"; only on an empty field
 * does it fall through to closing the dialog.
 */
export function escapeClearsSearch(event: KeyLike, query: string): boolean {
  return isDialogEscape(event) && query !== "";
}
