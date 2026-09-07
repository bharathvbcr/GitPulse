/** Minimal attribute surface shared by real Elements and test fixtures. */
export interface TipHost {
  getAttribute(qualifiedName: string): string | null;
  setAttribute(qualifiedName: string, value: string): void;
  removeAttribute(qualifiedName: string): void;
  readonly textContent: string | null;
}

/**
 * Every attribute the global tooltip treats as an anchor. Exported so the
 * renderer and the resolver cannot drift onto different selectors — a guide
 * card that the hover never finds is a labelled tab that still looks empty.
 */
export const TOOLTIP_ANCHOR_SELECTOR = "[title], [data-tip-text], [data-tip-guide]";

/** Pointer/focus target that can resolve a global-tooltip anchor. */
export interface TooltipPointerTarget extends TipHost {
  readonly nodeName?: string;
  hasAttribute?(qualifiedName: string): boolean;
  closest(selectors: string): unknown;
}

function declaresAccessibleName(el: TipHost): boolean {
  const label = el.getAttribute("aria-label");
  if (label !== null && label.trim().length > 0) return true;
  if (el.getAttribute("aria-labelledby") !== null) return true;
  return el.textContent !== null && el.textContent.trim().length > 0;
}

/**
 * Destination-guide key on an element, or null when it is a plain-text tip.
 *
 * Empty or whitespace-only values are treated as absent: a button that
 * declared `data-tip-guide=""` must not steal hover from a titled parent.
 */
export function tipGuideOf(el: TipHost): string | null {
  const key = el.getAttribute("data-tip-guide");
  if (key === null) return null;
  const trimmed = key.trim();
  return trimmed.length > 0 ? trimmed : null;
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

function isTooltipPointerTarget(value: unknown): value is TooltipPointerTarget {
  return (
    typeof value === "object" &&
    value !== null &&
    "getAttribute" in value &&
    typeof (value as TipHost).getAttribute === "function" &&
    "closest" in value &&
    typeof (value as TooltipPointerTarget).closest === "function"
  );
}

function isCanvasPointerTarget(el: TooltipPointerTarget): boolean {
  return (el.nodeName ?? "").toUpperCase() === "CANVAS";
}

function isSelfTitled(el: TooltipPointerTarget): boolean {
  if (typeof el.hasAttribute === "function") {
    return (
      el.hasAttribute("title") ||
      el.hasAttribute("data-tip-text") ||
      el.hasAttribute("data-tip-guide")
    );
  }
  return (
    el.getAttribute("title") !== null ||
    el.getAttribute("data-tip-text") !== null ||
    el.getAttribute("data-tip-guide") !== null
  );
}

function asTipHost(value: unknown): TipHost | null {
  if (
    typeof value === "object" &&
    value !== null &&
    "getAttribute" in value &&
    typeof (value as TipHost).getAttribute === "function"
  ) {
    return value as TipHost;
  }
  return null;
}

function suppressNativeTitle(el: TipHost): void {
  if (el.getAttribute("title") !== null) el.removeAttribute("title");
}

/**
 * Resolves the global-tooltip anchor for a pointer or focus target.
 *
 * Canvas graph nodes render GraphNodeTooltip themselves. Walking `closest`
 * from a canvas would bind a titled ancestor (the horizontally-scrollable
 * gutter) and replace commit details with a layout hint. Canvases only
 * participate when they carry their own title.
 *
 * Destination-guide anchors (`data-tip-guide`) win over a leftover `title`
 * so the OS bubble cannot flash beside the card, and so a labelled view tab
 * is never reduced to repeating its own name.
 */
export function tooltipAnchorFromTarget(target: unknown): TipHost | null {
  if (!isTooltipPointerTarget(target)) return null;
  const el = isCanvasPointerTarget(target)
    ? isSelfTitled(target)
      ? target
      : null
    : asTipHost(target.closest(TOOLTIP_ANCHOR_SELECTOR));
  if (!el) return null;
  if (tipGuideOf(el)) {
    suppressNativeTitle(el);
    return el;
  }
  return tipTextOf(el).trim().length > 0 ? el : null;
}
