import { clampMenuPosition } from "../branches/menuPosition";

export interface TourRect { left: number; top: number; width: number; height: number }

/** Prefer space below the control, then above it; keep the card inside the viewport. */
export function tourPosition(target: TourRect | null, card: { width: number; height: number }, width: number, height: number) {
  const gap = 16;
  const top = target
    ? target.top + target.height + card.height + gap * 2 <= height
      ? target.top + target.height + gap
      : Math.max(gap, target.top - card.height - gap)
    : height - card.height - gap;
  return clampMenuPosition(target?.left ?? width - card.width - gap, top,
    card.width, card.height, Math.max(0, width - gap), Math.max(0, height - gap));
}

/** A frame is a coalescing opportunity, not a guarantee. An occluded or
 * minimised window, a background tab and a headless runner all leave
 * `requestAnimationFrame` callbacks queued indefinitely, and a measurement that
 * only ever arrives on a frame then never arrives at all: the tour draws no
 * spotlight and tells the reader "The Open menu is not currently visible" about
 * a control that is on screen — a claim it has not measured. Bound the wait, so
 * a host that has stopped painting still reaches the measurement. */
export const FRAME_FALLBACK_MS = 50;

/** Observe only while a live tour step is mounted; never poll or leave listeners behind. */
export function observeTourTarget(selector: string, card: HTMLElement,
  update: (rect: TourRect | null, position: { left: number; top: number }) => void) {
  let target: HTMLElement | null = null;
  let frame = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let signature = "";
  const resize = new ResizeObserver(schedule);
  resize.observe(card);

  function measure() {
    const next = Array.from(document.querySelectorAll<HTMLElement>(selector)).find(el =>
      el.getClientRects().length > 0 && getComputedStyle(el).visibility !== "hidden") ?? null;
    if (next !== target) {
      if (target) resize.unobserve(target);
      target = next;
      if (target) {
        resize.observe(target);
        target.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
      }
    }
    const box = target?.getBoundingClientRect();
    const left = Math.max(0, box?.left ?? 0), top = Math.max(0, box?.top ?? 0);
    const width = Math.max(0, Math.min(innerWidth, box?.right ?? 0) - left);
    const height = Math.max(0, Math.min(innerHeight, box?.bottom ?? 0) - top);
    const rect = width > 0 && height > 0 ? { left, top, width, height } : null;
    const position = tourPosition(rect, card.getBoundingClientRect(), innerWidth, innerHeight);
    const nextSignature = JSON.stringify([rect, position]);
    if (signature !== nextSignature) { signature = nextSignature; update(rect, position); }
  }
  /** Whichever of the two arrives first measures; the other is cancelled. */
  function run() {
    cancelAnimationFrame(frame);
    if (timer !== null) clearTimeout(timer);
    frame = 0; timer = null;
    measure();
  }
  function schedule() {
    if (frame || timer !== null) return;
    frame = requestAnimationFrame(run);
    timer = setTimeout(run, FRAME_FALLBACK_MS);
  }
  const mutations = new MutationObserver(schedule);
  mutations.observe(document.body, { childList: true, subtree: true, attributes: true,
    attributeFilter: ["hidden", "class", "data-tour"] });
  window.addEventListener("resize", schedule);
  window.addEventListener("scroll", schedule, true);
  schedule();
  return () => {
    cancelAnimationFrame(frame);
    if (timer !== null) clearTimeout(timer);
    resize.disconnect(); mutations.disconnect();
    window.removeEventListener("resize", schedule);
    window.removeEventListener("scroll", schedule, true);
  };
}
