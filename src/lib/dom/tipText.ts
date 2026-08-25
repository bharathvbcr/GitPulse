/** Minimal attribute surface shared by real Elements and test fixtures. */
export interface TipHost {
  getAttribute(qualifiedName: string): string | null;
  setAttribute(qualifiedName: string, value: string): void;
  removeAttribute(qualifiedName: string): void;
  readonly textContent: string | null;
}

function declaresAccessibleName(el: TipHost): boolean {
  const label = el.getAttribute("aria-label");
  if (label !== null && label.trim().length > 0) return true;
  if (el.getAttribute("aria-labelledby") !== null) return true;
  return el.textContent !== null && el.textContent.trim().length > 0;
}

/**
 * Tooltip text for an element, migrating a native `title` on first sight.
 *
 * Migration moves the text into `data-tip-text` (which the styled tooltip
 * reads) and removes `title` to suppress the OS-native bubble. Because an
 * icon-only control's accessible name often lives in that `title`, it is
 * mirrored into `aria-label` first whenever the element names itself no
 * other way — otherwise the migration would silently erase its name from
 * assistive technology.
 *
 * Idempotent: once migrated, the element carries no `title` and later calls
 * read straight from `data-tip-text`.
 */
export function tipTextOf(el: TipHost): string {
  const migrated = el.getAttribute("data-tip-text");
  if (migrated !== null) return migrated;

  const native = el.getAttribute("title");
  if (native === null) return "";

  el.setAttribute("data-tip-text", native);
  if (native.trim().length > 0 && !declaresAccessibleName(el)) {
    el.setAttribute("aria-label", native);
  }
  el.removeAttribute("title");
  return native;
}
