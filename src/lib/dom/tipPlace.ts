/**
 * Viewport placement for the global tooltip.
 *
 * The bubble must not share vertical space with its anchor when either side
 * of the anchor can hold it: a tip that opens downward from a toolbar button
 * was covering the first line of the file under that button. `prefer` is the
 * side to try first (`data-tip-place`); the other side is the fallback when
 * the preferred one does not fit. When neither fits, the side with more free
 * pixels wins and the box is clamped inside the viewport. Every non-finite
 * input fails closed to a finite on-screen coordinate.
 */

export type TipSide = "above" | "below";

export interface TipRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

export interface PlacedTip {
  left: number;
  top: number;
  side: TipSide;
}

const GAP = 8;
const MARGIN = 8;

function finite(value: number, fallback = 0): number {
  return Number.isFinite(value) ? value : fallback;
}

export function preferredTipSide(value: string | null | undefined): TipSide {
  return value?.trim() === "above" ? "above" : "below";
}

export function placeTooltipBubble(
  anchor: TipRect,
  bubble: { width: number; height: number },
  viewport: { width: number; height: number },
  prefer: TipSide = "below",
): PlacedTip {
  const vw = Math.max(0, finite(viewport.width));
  const vh = Math.max(0, finite(viewport.height));
  const bw = Math.max(0, finite(bubble.width));
  const bh = Math.max(0, finite(bubble.height));
  const ax = finite(anchor.left);
  const ay = finite(anchor.top);
  const aw = Math.max(0, finite(anchor.width));
  const ah = Math.max(0, finite(anchor.height));

  const maxLeft = Math.max(MARGIN, vw - bw - MARGIN);
  const maxTop = Math.max(MARGIN, vh - bh - MARGIN);
  const rawLeft = ax + aw / 2 - bw / 2;
  const left = Math.min(Math.max(MARGIN, finite(rawLeft, MARGIN)), maxLeft);

  const belowTop = ay + ah + GAP;
  const aboveTop = ay - bh - GAP;
  const fits = (top: number) => top >= MARGIN && top + bh <= vh - MARGIN;
  const fitsBelow = fits(belowTop);
  const fitsAbove = fits(aboveTop);

  // A toolbar asks for "above" because the document starts immediately under
  // the control. When the viewport has no room above, dropping below covers
  // that document. Sit beside the control, in its own band, instead.
  if (prefer === "above" && !fitsAbove) {
    const besideLeft = ax + aw + GAP;
    const fitsRight = besideLeft + bw <= vw - MARGIN;
    const rawLeft = fitsRight ? besideLeft : ax - bw - GAP;
    const left = Math.min(Math.max(MARGIN, finite(rawLeft, MARGIN)), maxLeft);
    const bandTop = Math.max(MARGIN, Math.min(ay + (ah - bh) / 2, ay + ah - bh));
    const top = Math.min(Math.max(MARGIN, finite(bandTop, MARGIN)), maxTop);
    return {
      left: Number.isFinite(left) ? left : MARGIN,
      top: Number.isFinite(top) ? top : MARGIN,
      side: "above",
    };
  }

  let side: TipSide;
  if (prefer === "above" ? fitsAbove : fitsBelow) side = prefer;
  else if (prefer === "above" ? fitsBelow : fitsAbove) side = prefer === "above" ? "below" : "above";
  else side = ay > vh - (ay + ah) ? "above" : "below";

  const rawTop = side === "above" ? aboveTop : belowTop;
  const top = Math.min(Math.max(MARGIN, finite(rawTop, MARGIN)), Number.isFinite(maxTop) ? maxTop : MARGIN);

  return {
    left: Number.isFinite(left) ? left : MARGIN,
    top: Number.isFinite(top) ? top : MARGIN,
    side,
  };
}

/** True when the placed box intersects the anchor. Side-by-side is not a cover. */
export function tipCoversAnchor(
  placed: PlacedTip,
  anchor: TipRect,
  bubble: { width: number; height: number },
): boolean {
  const vertical = placed.top < anchor.top + anchor.height && placed.top + bubble.height > anchor.top;
  const horizontal = placed.left < anchor.left + anchor.width && placed.left + bubble.width > anchor.left;
  return vertical && horizontal;
}
