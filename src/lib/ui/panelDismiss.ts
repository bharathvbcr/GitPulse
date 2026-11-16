import { eventTargetElement } from "./dismiss";
import type { PopoverDismissOptions } from "./popover";

/**
 * What counts as INSIDE a floating sidebar or dock for outside-click dismiss.
 *
 * The panel itself, plus any floating layer that something in it opened. Those
 * layers are portaled to <body> (`dom/portal`) so that `.gp-pane`'s paint
 * containment does not clip them, which puts them outside the panel's subtree:
 * containment alone would read a click on one of its own menus as "outside" and
 * close the panel under it. There is no ownership registry to ask (a portaled
 * node keeps no link to its opener), and the popover owner, `ui/popover.ts`,
 * already takes a selector for exactly this, so the answer is a selector over
 * the roles and classes every floating layer in the app wears: menus
 * (`gp-menu`, `role="menu"`), popovers (`gp-pop`), listboxes, dialogs and
 * their scrims, tooltips.
 *
 * The trade is deliberate: a click on any open floating layer, an unrelated
 * one included, leaves the panel open. Closing a panel is cheap to repeat;
 * closing it on the click that was choosing from its own menu is not.
 */
export const FLOATING_LAYER_SELECTOR = [
  ".gp-menu",
  ".gp-pop",
  ".gp-scrim",
  '[role="menu"]',
  '[role="listbox"]',
  '[role="dialog"]',
  '[role="alertdialog"]',
  '[role="tooltip"]',
].join(", ");

/**
 * The `dismiss` options for a floating panel: a pointerdown on capture (so a
 * `stopPropagation` further down cannot keep it open), with the panel, its
 * trigger if it has one, and every floating layer counted as inside. Escape is
 * left to the panel's own handler.
 */
export function floatingPanelDismiss(panelSelector: string, triggerSelector?: string): PopoverDismissOptions {
  return {
    inside: [panelSelector, triggerSelector, FLOATING_LAYER_SELECTOR].filter(Boolean).join(", "),
    pointer: "pointerdown",
    escape: "none",
  };
}

/**
 * Text fields. A double-click in one selects a word; the reader is already
 * working in the page, so it is not a request to clear the way to it.
 */
const EDITABLE_SELECTOR = ['input', 'textarea', 'select', '[contenteditable]:not([contenteditable="false"])'].join(", ");

const DRAG_REGION_ATTR = "data-tauri-drag-region";
const CLICKABLE_TAGS = new Set(["A", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "LABEL", "SUMMARY"]);
const INTERACTIVE_ROLES = new Set(["button", "link", "menuitem", "tab", "checkbox", "radio", "switch", "option"]);

interface AttributeHost {
  tagName?: unknown;
  getAttribute(name: string): string | null;
  hasAttribute(name: string): boolean;
}

function isAttributeHost(value: unknown): value is AttributeHost {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AttributeHost).getAttribute === "function" &&
    typeof (value as AttributeHost).hasAttribute === "function"
  );
}

function isClickable(el: AttributeHost): boolean {
  const tag = typeof el.tagName === "string" ? el.tagName.toUpperCase() : "";
  return (
    CLICKABLE_TAGS.has(tag) ||
    (el.hasAttribute("contenteditable") && el.getAttribute("contenteditable") !== "false") ||
    (el.hasAttribute("tabindex") && el.getAttribute("tabindex") !== "-1") ||
    INTERACTIVE_ROLES.has(el.getAttribute("role") ?? "")
  );
}

/**
 * True when the window itself takes this double-click: Tauri's drag script
 * zooms (maximizes) the window on a double-click in a drag region, and a
 * double-click that resizes the window is not also a request to hide a panel.
 *
 * Mirrors `isDragRegion` in Tauri's `src/window/scripts/drag.js` (the fork in
 * src-tauri/framework/tauri), walking `event.composedPath()` from the target
 * up: a clickable element without the attribute blocks it, `"false"` blocks
 * it, `"deep"` takes its whole subtree, and a bare or `"true"` attribute takes
 * only a direct hit. A copy rather than a call: the script is injected into
 * the page and exports nothing.
 */
export function isWindowDragDoubleClick(path: readonly unknown[]): boolean {
  const target = path[0];
  for (const el of path) {
    if (!isAttributeHost(el)) continue;
    const attr = el.getAttribute(DRAG_REGION_ATTR);
    if (attr === null) {
      if (isClickable(el)) return false;
      continue;
    }
    if (attr === "false") return false;
    if (attr === "deep") return true;
    if (attr === "" || attr === "true") return el === target;
  }
  return false;
}

/** Where a double-click asks a docked panel to get out of the way. */
export interface PageDoubleClickScope {
  /** The panel itself. */
  panel: string;
  /** The panel's own toggle: its two clicks already decided, so the double-click must not undo them. */
  trigger?: string;
  /** The column the panel shares with the page; anything it contains is the page. */
  column: { contains(node: unknown): boolean } | null;
  /** Further page chrome outside that column, such as the page tab bar. */
  page?: string;
}

/**
 * True when a double-click landed on the page beside a docked panel, or on
 * the page's tab bar, which asks for the panel to get out of the way. Not for
 * one inside the panel (that selects a word of its own content), on its
 * trigger, on a floating layer something opened, in a text field, or anywhere
 * outside the page (the sidebar). A missing target hides nothing: unlike an
 * outside-click dismiss, a double-click is a request, and an unknown one is not.
 */
export function pageDoubleClickHidesPanel(target: unknown, scope: PageDoubleClickScope): boolean {
  const el = eventTargetElement(target);
  if (!el) return false;
  const onPage = scope.column?.contains(el) === true || (scope.page !== undefined && el.closest(scope.page) != null);
  if (!onPage) return false;
  const kept = [scope.panel, scope.trigger, FLOATING_LAYER_SELECTOR, EDITABLE_SELECTOR].filter(Boolean).join(", ");
  return el.closest(kept) == null;
}
