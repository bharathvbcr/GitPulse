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
