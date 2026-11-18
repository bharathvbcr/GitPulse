/**
 * Pure container-adaptive layout resolver for `MarkDevViewer`.
 *
 * `MarkDevViewer` renders inside full-tab Code views as well as narrow
 * terminal file splits and docked sidebars, so layout decisions are computed
 * from the viewer's own measured viewport box rather than window breakpoints.
 */

export const MARKDEV_OUTLINE_RAIL_WIDTH = 224;
export const MARKDEV_FLOAT_CARD_GUTTERS = 12;
export const MARKDEV_OUTLINE_FOOTPRINT =
  MARKDEV_OUTLINE_RAIL_WIDTH + MARKDEV_FLOAT_CARD_GUTTERS;
export const MARKDEV_MIN_CONTENT_BESIDE_OUTLINE = 340;
export const MARKDEV_OUTLINE_RAIL_MIN_WIDTH =
  MARKDEV_OUTLINE_FOOTPRINT + MARKDEV_MIN_CONTENT_BESIDE_OUTLINE;

export const MARKDEV_SPLIT_HORIZONTAL_MIN_WIDTH = 620;
export const MARKDEV_SPLIT_MIN_PANE_WIDTH = 260;

/**
 * Narrowest viewer that keeps the Rendered / Split / Raw switcher in the one
 * toolbar row. The row's smallest form (logo, outline count, the labelled
 * switcher and the three icon actions) measures about 477px in Chromium, so
 * below this the switcher takes a row of its own: the control that changes
 * what the pane shows is never scrolled out of reach behind the toolbar's
 * overflow. The toolbar's label breakpoints in MarkDevViewer are set so each
 * step up still fits; the markdown harness checks the row at every step.
 */
export const MARKDEV_TOOLBAR_INLINE_MIN_WIDTH = 512;

export type MarkDevSplitDirection = "horizontal" | "vertical";
export type MarkDevToolbarLayout = "inline" | "stacked";
export type MarkDevOutlineLayout = "rail" | "sheet";
export type MarkDevLayoutMode = "unmeasured" | "wide" | "compact";

export interface MarkDevLayoutInput {
  readonly width: number;
  readonly outlineRequested: boolean;
  readonly hasOutline: boolean;
}

export interface MarkDevLayout {
  readonly mode: MarkDevLayoutMode;
  readonly outlineVisible: boolean;
  readonly outlineLayout: MarkDevOutlineLayout;
  readonly contentWidth: number;
  readonly splitDirection: MarkDevSplitDirection;
  readonly toolbar: MarkDevToolbarLayout;
}

/**
 * Resolves outline placement, split axis and toolbar placement from the
 * measured width of `MarkDevViewer`.
 *
 * Height decides nothing. A stacked split gives its two panes equal shares of
 * whatever height there is and each scrolls itself; it is never one scroller
 * around two minimum-height panes, because every pane scroller sets
 * `overscroll-behavior: contain` (app.css), so a wheel over a pane never
 * reaches an outer scroller and the lower pane would sit out of reach.
 */
export function resolveMarkDevLayout(input: MarkDevLayoutInput): MarkDevLayout {
  const validWidth = Number.isFinite(input.width) && input.width > 0;
  const outlineVisible = Boolean(input.outlineRequested && input.hasOutline);

  if (!validWidth) {
    return {
      mode: "unmeasured",
      outlineVisible,
      outlineLayout: "rail",
      contentWidth: 0,
      splitDirection: "horizontal",
      toolbar: "inline",
    };
  }

  const outlineLayout: MarkDevOutlineLayout =
    input.width >= MARKDEV_OUTLINE_RAIL_MIN_WIDTH ? "rail" : "sheet";
  const contentWidth =
    outlineVisible && outlineLayout === "rail"
      ? Math.max(0, input.width - MARKDEV_OUTLINE_FOOTPRINT)
      : input.width;
  const splitDirection: MarkDevSplitDirection =
    contentWidth >= MARKDEV_SPLIT_HORIZONTAL_MIN_WIDTH ? "horizontal" : "vertical";
  const mode: MarkDevLayoutMode =
    splitDirection === "horizontal" && outlineLayout === "rail" ? "wide" : "compact";

  return {
    mode,
    outlineVisible,
    outlineLayout,
    contentWidth,
    splitDirection,
    toolbar: input.width >= MARKDEV_TOOLBAR_INLINE_MIN_WIDTH ? "inline" : "stacked",
  };
}
